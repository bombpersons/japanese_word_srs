use std::collections::{HashMap, HashSet};
use chrono::{DateTime, TimeZone, NaiveDateTime, Utc};

use lindera::tokenizer::Tokenizer;
use rusqlite::{Connection, DatabaseName, params};

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
                    reviewed INTEGER DEFAULT FALSE,
                    next_review_at TEXT,
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

    fn review_word(&self, word: &str) {
        // For now, just schedule another review
        let word_id: i64 = self.db_conn.query_row(
            "SELECT id, text
            FROM words
                WHERE text = ?", [&word], |row| row.get(0)
        ).unwrap();

        let next_review_time = 
            format!("{}", Utc::now() + chrono::Duration::minutes(10));

        self.db_conn.execute(
            "UPDATE words
            SET reviewed = TRUE,
                next_review_at = ?
            WHERE
                id = ?", params!(next_review_time, &word_id)
        ).unwrap();
    }

    fn get_word_to_review(&self) -> String {
        // Find the word that's review expired most recently.
        let word: String;
        {
            let now_time = 
                format!("{}", Utc::now() + chrono::Duration::minutes(10));

            word = self.db_conn.query_row(
                "SELECT reviewed, next_review_at, text FROM words
                WHERE reviewed = TRUE
                    AND next_review_at < ?
                ORDER BY next_review_at ASC
                LIMIT 1", params!(now_time),
                |row| row.get(2)
            ).unwrap();
        }

        word
    }
}

struct Knowledge {
    words_seen: HashMap<String, i32>,
}

fn main() -> () {
    // tokenize the text
    let test_text = include_str!("big_test.txt");
    
    let mut knowledge = KnowledgeDB::new("test.sqlite");

    // // Split by sentences and add each one seperately.
    // iterate_sentences(test_text, |sentence| {
    //     // First add the sentence.
    //     knowledge.add_sentence(sentence);
    // });

    knowledge.review_word("考える");
    let word = knowledge.get_word_to_review();
    println!("Next word to review is {}", word);

    // let sentences = knowledge.get_sentences_for_word("考える");
    // for sentence in sentences {
    //     println!("{}", sentence);
    // }
}