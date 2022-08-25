use std::{collections::{HashMap, HashSet}, env, fs::File, io::{Read, self}};
use chrono::{DateTime, TimeZone, NaiveDateTime, Utc, Duration, ParseResult};

use lindera::tokenizer::Tokenizer;
use rusqlite::{Connection, DatabaseName, params};

// https://supermemo.guru/wiki/SuperMemo_1.0_for_DOS_(1987)#Algorithm_SM-2
struct SuperMemoItem {
    repitition: u32,
    duration: u32,
    e_factor: f32
}

fn super_memo_2(item: SuperMemoItem, response_quality: f32) -> SuperMemoItem {
    let repitition = if response_quality < 3.0 { 0 } else { item.repitition };

    match repitition {
        0 => SuperMemoItem {
             repitition: 1,
             duration: 1,
             e_factor: item.e_factor
        },
        1 => SuperMemoItem {
            repitition: 2,
            duration: 6,
            e_factor: item.e_factor
        },
        r => {
            let e_factor = (item.e_factor + (0.1 - (5.0 - response_quality) * (0.08 + (5.0 - response_quality) * 0.02))).max(1.3);
            let duration = ((item.duration as f32) * e_factor).ceil() as u32;
            let repitition = repitition + 1;

            SuperMemoItem {
                repitition,
                duration,
                e_factor
            }
        }
    }
}

struct WordFrequencyList {
    words: HashMap<String, i64>
}

impl WordFrequencyList {
    fn new() -> Self {
        let wordlist = include_str!("japanese_word_frequency.txt");
        let mut words = HashMap::new();
        for (index, line) in wordlist.lines().enumerate() {
            words.insert(line.to_string(), index as i64);
        }

        Self { 
            words
        }
    }

    fn get_word_freq(&self, word: &str) -> i64 {
        match self.words.get(word) {
            Some(freq) => *freq,
            None => i64::MAX // If it's not on the list if must be very infrequent.
        }
    }
}

fn iterate_sentences<F>(text: &str, mut func: F) where
    F: FnMut(&str) {

    let terminators: HashSet<char> = HashSet::from(['。', '\n', '！', '？']);
    let open_quotes: HashSet<char> = HashSet::from(['「']);
    let close_quotes: HashSet<char> = HashSet::from(['」']);

    let mut depth: i32 = 0;
    let mut cur_string: String = String::new();
    for c in text.chars() {
        cur_string.push(c);

        if open_quotes.contains(&c) {
            depth += 1;
        }
        else if close_quotes.contains(&c) {
            depth -= 1;
        }
        else if depth == 0 && terminators.contains(&c) {
            let sentence = cur_string.trim();

            if !sentence.is_empty() {
                func(sentence);
            }

            cur_string.clear();
        }
    }
}

struct KnowledgeDB {
    tokenizer: Tokenizer,
    word_frequency_list: WordFrequencyList,

    db_conn: Connection,
}

impl KnowledgeDB {
    fn new(db_path: &str) -> Self {
        // Create the tokenizer.
        let tokenizer = Tokenizer::new().unwrap();

        // Create the databse connection.
        let db_conn = Connection::open(db_path).unwrap();
        db_conn.pragma_update(Some(DatabaseName::Main), "foreign_keys", true).unwrap();

        // Table for sentences.
        db_conn.execute(
            "CREATE TABLE IF NOT EXISTS sentences (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    text TEXT NOT NULL,
                    UNIQUE(text)
                )", []).unwrap();

        // Table for words.
        db_conn.execute(
            "CREATE TABLE IF NOT EXISTS words (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    text TEXT NOT NULL,
                    count INTEGER DEFAULT 1,
                    frequency INTEGER,

                    reviewed INT DEFAULT 0,
                    next_review_at TEXT,

                    review_duration INTEGER DEFAULT 0,
                    e_factor REAL DEFAULT 0,
                    repitition INTEGER DEFAULT 0,

                    UNIQUE(text)
                )", []).unwrap();
        
        // Many to Many link between words and sentences.
        db_conn.execute(
            "CREATE TABLE IF NOT EXISTS word_sentence (
                    word_id INTEGER NOT NULL REFERENCES words(id) ON DELETE CASCADE,
                    sentence_id INTEGER NOT NULL REFERENCES sentences(id) ON DELETE CASCADE,
                    PRIMARY KEY (word_id, sentence_id)
                )", []).unwrap();

        db_conn.execute(
            "CREATE INDEX IF NOT EXISTS sentence_index ON word_sentence(sentence_id)", []).unwrap();

        db_conn.execute(
            "CREATE INDEX IF NOT EXISTS word_index ON word_sentence(word_id)", []).unwrap();

        Self {
            tokenizer,
            word_frequency_list: WordFrequencyList::new(),
            db_conn
        }
    }

    fn add_sentence(&mut self, sentence: &str) {
        //println!("Adding sentence '{}' to database.", sentence);

        // Tokenize the sentence to get the words.
        let tokens = self.tokenizer.tokenize(sentence).unwrap();
        let mut words = Vec::<String>::new();
        for token in tokens {
            if token.detail.len() > 7 {
                let base_form = &token.detail[6];
                words.push(base_form.to_string());
            }
        }

        // Insert the sentence and words into the database.
        let tx = self.db_conn.transaction().unwrap();
        if tx.execute(
            "INSERT OR IGNORE INTO sentences(text)
                VALUES(?);", [sentence]).unwrap() == 1 {
            
            let sentence_id = tx.last_insert_rowid();

            // We inserted the sentence, so let's add the words too.
            for word in words {
                //println!("Adding word '{}' to database.", word);

                // Find some info to add to the word.
                let frequency = self.word_frequency_list.get_word_freq(&word);

                // Add the word.
                tx.execute(
                    "INSERT INTO words(count, frequency, text)
                    VALUES(1, ?, ?)
                    ON CONFLICT(text) DO UPDATE SET count=count + 1", params!(frequency, &word)).unwrap(); 

                let word_id: i64 = tx.query_row(
                    "SELECT id, text
                    FROM words
                        WHERE text = ?", [&word], |row| row.get(0)
                ).unwrap();

                // Add the relationship word->sentence
                tx.execute(
                    "INSERT OR IGNORE INTO word_sentence(word_id, sentence_id)
                    VALUES(?, ?);", params![word_id, sentence_id]).unwrap();
            }
        }
        tx.commit().unwrap();
    }

    fn get_sentences_for_word(&self, word: &str) -> Vec<String> {
        // Find the id for the word.
        let query: Result<i64, rusqlite::Error>  = self.db_conn.query_row(
            "SELECT id, text
            FROM words
                WHERE text = ?", [word], |row| row.get(0));
        match query {
            Ok(word_id) => {
                // Find all the sentences that include that word.
                let mut statement = self.db_conn.prepare(
                    "SELECT word_id, sentence_id, sentences.id, sentences.text
                    FROM word_sentence
                        INNER JOIN sentences ON sentence_id = sentences.id
                    WHERE word_id = ?"
                ).unwrap();

                // Create a vec.
                let mut sentences_vec = Vec::new();
                let sentences = statement.query_map([word_id], |row| row.get(3)).unwrap();
                for sentence in sentences {
                    sentences_vec.push(sentence.unwrap());
                }

                sentences_vec
            },
            Err(e) => {
                println!("{}", e);
                Vec::<String>::new()
            }
        }
    }

    fn review_word(&mut self, word: &str, response_quality: f32) {
        // For now, just schedule another review
        let result = self.db_conn.query_row(
            "SELECT id, text, repitition, e_factor, review_duration
            FROM words
                WHERE text = ?", [&word], |row| {
                    let word_id: i64 = row.get(0)?;
                    let repitition: u32 = row.get(2)?;
                    let e_factor: f32 = row.get(3)?;
                    let duration: u32 = row.get(4)?;

                    Ok((word_id, repitition, e_factor, duration))
                }
        );

        match result {
            Ok((word_id, repitition, e_factor, duration)) => {
                // Calculate how long till the next review.
                let mut sm = SuperMemoItem {
                    repitition,
                    e_factor,
                    duration
                };
                sm = super_memo_2(sm, response_quality);

                // Store it in the database.
                let next_review_time = format!("{}", Utc::now() + chrono::Duration::days(sm.duration as i64));
                self.db_conn.execute(
                    "UPDATE words
                    SET repitition = ?,
                        e_factor = ?,
                        review_duration  = ?,
                        next_review_at = ?,
                        reviewed = TRUE
                    WHERE
                        id = ?", params!(sm.repitition, sm.e_factor, sm.duration, next_review_time, &word_id)
                ).unwrap();
            },
            Err(e) => println!("Error getting review data from database for word {}. Error: {}", word, e)
        }
    }

    fn get_sentence_to_review(&self, word: &str) -> Option<String> {
        // Find the id of the word.
        let review_word_id: i64 = self.db_conn.query_row(
            "SELECT id, text
            FROM words
                WHERE text = ?", [word], |row| row.get(0)
        ).expect(format!("Error finding word {} in database", word).as_str());

        // Find all the sentences that include that word.
        let mut statement = self.db_conn.prepare(
            "SELECT word_id, sentence_id, sentences.id, sentences.text
            FROM word_sentence
                INNER JOIN sentences ON sentence_id = sentences.id
            WHERE word_id = ?"
        ).expect(format!("Error finding sentences containing word {}", word).as_str());

        // Go through each sentence returned and calculate a heuristic that represents
        // how much knowledge contained within the sentence is unknown to the user (excluding the word to be reviewed).
        // More infrequent words will have a higher cost.
        let sentence_ids = statement.query_map([review_word_id], |row| row.get(1))
            .expect(format!("Error getting sentences containing word {}", word).as_str());

        // Store the current fittest sentence.
        let mut fittest_sentence = None;
            
        for sentence_id_result in sentence_ids {
            let sentence_id: i64 = sentence_id_result.expect(format!("Couldn't retrieve sentence for word {}", word).as_str());

            // Find all the words associated with the sentence.
            let mut statement = self.db_conn.prepare(
                "SELECT word_id, sentence_id, words.frequency FROM word_sentence
                INNER JOIN words ON word_id = words.id
                WHERE sentence_id = ?"
            ).expect(format!("Error finding sentences containing word {}", word).as_str());

            // TODO: This should take into account words that the user already knows (has reviewed).
            // It should make words that the user has low e_factors or long durations cost less
            let mut total = 0.0;
            let word_ids = statement.query_map([sentence_id], |row| {
                let id: i64 = row.get(0)?;
                let freq: i64 = row.get(2)?;
                Ok((id, freq))
            }).expect(format!("Couldn't get words contained in sentence {}", sentence_id).as_str());
            for word_id_result in word_ids {
                let (word_id, word_freq) = word_id_result.expect(format!("Error getting word id for word in potential sentence for review.").as_str());
            
                // If the word is the word we are reviewing then don't add this to the total.
                if word_id != review_word_id {
                    total += word_freq as f64;

                    println!("WORDS: {} costs {}", word_id, word_freq);
                }
            }

            println!("SENTENCE TOTAL: {} costs {}", sentence_id, total);

            // Store the sentence info if it's fitter than the one we have stored already.
            match fittest_sentence {
                Some((_, cost)) => {
                    if total < cost {
                        fittest_sentence = Some((sentence_id, total));
                    }
                },
                None => {
                    fittest_sentence = Some((sentence_id, total));
                }
            }
        }

        // Get the sentence text.
        match fittest_sentence {
            Some((id, cost)) => {
                println!("Picked sentence cost {}", cost);
                Some(self.db_conn.query_row(
                    "SELECT id, text
                    FROM sentences
                        WHERE id = ?", [id], |row| row.get(1)
                ).expect(format!("Couldn't get sentence text for {}.", id).as_str()))
            },
            None => None
        }
    }

    fn get_word_to_review(&self) -> Option<String> {
        // Find the word that's review expired most recently.
        let mut word = None;
        {
            let now_time = 
                format!("{}", Utc::now());

            match self.db_conn.query_row(
                "SELECT repitition, next_review_at, text FROM words
                WHERE reviewed = TRUE
                    AND next_review_at < ?
                ORDER BY next_review_at ASC
                LIMIT 1", params!(now_time),
                |row| row.get(2)
            ) {
                Ok(w) => word = Some(w),
                Err(e) => println!("No scheduled word to review!")
            }
        }

        // If there wasn't a word to review found,
        // find a new word that hasn't been reviewed yet.
        // Find the word that has the lowest frequency rank.
        if word.is_none() {
            match self.db_conn.query_row("
                SELECT text, frequency, reviewed FROM words
                WHERE reviewed = FALSE
                ORDER BY frequency ASC
                LIMIT 1", [], |row| row.get(0)
            ) {
                Ok(w) => word = Some(w),
                Err(e) => println!("No new words to review either!")
            }
        }

        word
    }
}

fn verb_search(knowledge: &KnowledgeDB, word: &str) {
    let sentences = knowledge.get_sentences_for_word(word);
    if sentences.is_empty() {
        println!("No sentences with the word {} found.", word);
    } else {
        println!("Showing {} results:", sentences.len());
        for sentence in sentences {
            println!("{}", sentence);
        }
    }
}

fn verb_review(knowledge: &mut KnowledgeDB) {
    let word = knowledge.get_word_to_review()
        .expect("There are no words, either scheduled or new to review.");

    let sentence = knowledge.get_sentence_to_review(word.as_str())
        .expect(format!("No sentence for the word {} could be found.", word).as_str());

    println!("{}", sentence);
    println!("Enter 0-5:");

    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer).unwrap();
    match buffer.trim().parse::<f32>() {
        Ok(quality) => {
            if quality > 5.0 || quality < 0.0 {
                println!("Response must be between 0 and 5")
            } else {
                knowledge.review_word(word.as_str(), quality)
            }
        },
        Err(_) => println!("Didn't enter a number! Please enter 0 - 5")
    }

    println!("Reviewed {}", word);
}

fn verb_add(knowledge: &mut KnowledgeDB, file_path: &str) {
    println!("Adding contents of {} to the database.", file_path);

    // Open the file.
    match File::open(file_path) {
        // Successfully opened the file.
        Ok(mut file) => {
            // Read the contents.
            let mut file_contents = String::new();
            match file.read_to_string(&mut file_contents) {
                Ok(bytes_read) => {
                    println!("Read {} bytes.", bytes_read);

                    // Iterate over the sentences and add them to our db.
                    iterate_sentences(file_contents.as_str(), |sentence| {
                        knowledge.add_sentence(sentence);
                    })
                },
                Err(err) => println!("Couldn't read contents of file {}, Error: {}", file_path, err)
            }
        },
        Err(err) => println!("Couldn't open file {}, Error: {}", file_path, err)
    }
}

fn parse_arguments(knowledge: &mut KnowledgeDB, args: &Vec<String>) {
    if args.len() < 2 {
        println!("No arguments were given.");
    }

    // Call the correct function.
    let verb = args[1].as_str();
    match verb {
        "search" => {
            if args.len() < 3 {
                println!("Usage: 'search {{word-to-search}}")
            } else {
                verb_search(knowledge, args[2].as_str())
            }
        },
        "review" => {
            verb_review(knowledge)
        },
        "add" => {
            if args.len() < 3 {
                println!("Usage: 'add {{path-to-file}}")
            } else {
                verb_add(knowledge, args[2].as_str())
            }
        }
        _ => println!("{}: Unknown command.", verb)
    }
}

fn main() -> () {
    // Open the database.
    let mut knowledge = KnowledgeDB::new("database.sqlite");

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    parse_arguments(&mut knowledge, &args);


    
    // // Split by sentences and add each one seperately.
    // iterate_sentences(test_text, |sentence| {
    //     // First add the sentence.
    //     knowledge.add_sentence(sentence);
    // });

    // knowledge.review_word("考える");
    // let word = knowledge.get_word_to_review();
    // println!("Next word to review is {}", word);

    // let sentences = knowledge.get_sentences_for_word("考える");
    // for sentence in sentences {
    //     println!("{}", sentence);
    // }
}