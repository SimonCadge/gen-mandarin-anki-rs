use std::{any::Any, error::Error, fmt, fs::File, io::Write, panic, path::PathBuf, time::{UNIX_EPOCH, SystemTime, Duration}};

use again::RetryPolicy;
use chinese_dictionary::{tokenize, query_by_chinese, WordEntry, ClassificationResult, classify};
use config::Config;
use futures::{future::join_all, FutureExt};
use genanki_rs::{Field, Model, Deck, Template, Note, Package};
use itertools::Itertools;
use log::{LevelFilter, info, warn, debug, trace};
use pinyin_parser::PinyinParser;
use pinyin_zhuyin::encode_zhuyin;
use reqwest::{Client, header::{HeaderMap, CONTENT_TYPE, AUTHORIZATION, HeaderValue, HeaderName}};
use serde::Deserialize;
use serde_json::{Value, json};
use simplelog::{CombinedLogger, TermLogger, WriteLogger, TerminalMode, ColorChoice};
use tokio::sync::OnceCell;
use rand::distributions::{Alphanumeric, DistString};

static CONFIG: OnceCell<GenankiConfig> = OnceCell::const_new();

#[derive(Debug, Deserialize)]
struct GenankiConfig {
    model: ModelConfig,
    azure: AzureConfig,
    openai: OpenAIConfig,
    mandarin: MandarinConfig,
}

#[derive(Debug, Deserialize)]
struct ModelConfig {
    word_model_id: i64,
    sentence_model_id: i64,
    deck_id: i64,
}

#[derive(Debug, Deserialize)]
struct AzureConfig {
    translator: AzureTranslatorConfig,
    speech: AzureSpeechConfig,
    region: String,
}

#[derive(Debug, Deserialize)]
struct AzureTranslatorConfig {
    key: String,
}

#[derive(Debug, Deserialize)]
struct AzureSpeechConfig {
    key: String,
    #[serde(default = "default_speech_api_voice_name")]
    voice_name: String,
    locale: String,
}

fn default_speech_api_voice_name() -> String {
    "zh-TW-YunJheNeural".to_string()
}

#[derive(Debug, Deserialize)]
struct OpenAIConfig {
    key: String,
    organisation: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MandarinConfig {
    #[serde(default)]
    script: MandarinScript,
    #[serde(default)]
    reading: MandarinReading,
}

#[derive(Debug, Deserialize, Default)]
enum MandarinScript {
    #[default]
    Traditional,
    Simplified,
}

impl fmt::Display for MandarinScript {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
       match self {
           MandarinScript::Traditional => write!(f, "Traditional Chinese"),
           MandarinScript::Simplified => write!(f, "Simplified Chinese"),
       }
    }
}

impl MandarinScript {
    fn build_language(&self) -> String {
        match self {
            MandarinScript::Traditional => "zh-Hant".to_string(),
            MandarinScript::Simplified => "zh-Hans".to_string(),
        }
    }

    fn build_from_script(&self) -> String {
        match self {
            MandarinScript::Traditional => "Hant".to_string(),
            MandarinScript::Simplified => "Hans".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
enum MandarinReading {
    #[default]
    Zhuyin,
    Pinyin,
}

fn parse_config() -> GenankiConfig {
    let config = Config::builder()
        .add_source(config::File::with_name("config"))
        .add_source(config::Environment::with_prefix("GENANKI"))
        .build()
        .unwrap();

    config.try_deserialize::<GenankiConfig>().unwrap()
}

#[derive(Debug)]
struct Token {
    text: String,
    word_entry: Option<Vec<&'static WordEntry>> //I believe this should only ever have one word entry inside, but I'm not certain.
}

impl Token {
    fn build_definition(&self) -> Option<String> { //Returns none if there is no word entry vec, or if the vec doesn't contain any english translation information.
        match &self.word_entry {
            Some(word_entry) => {
                let definition = word_entry.into_iter().flat_map(|word| &word.english).join(", ");
                match definition.len() {
                    0 => None,
                    _ => Some(definition),
                }
            },
            None => None,
        }
    }
    fn build_reading_allow_multiple(&self) -> Option<String> {
        match &self.word_entry {
            Some(word_entry) => {
                let reading = word_entry.into_iter().map(|word| word.derive_zhuyin()).join(",");
                match reading.len() {
                    0 => None,
                    _ => Some(reading),
                }
            },
            None => todo!(),
        }
    }
}

struct MandarinSentence {
    raw_sentence: String,
    tokens: Vec<Token>
}

impl MandarinSentence {
    fn build_plain_sentence(&self) -> String {
        self.tokens.iter().map(|token| match token.text.as_str() {
            "*" => String::from(""),
            _ => token.text.clone()
        }).join("")
    }
    fn build_note_sentence(&self) -> String {
        let mut have_seen_star = false;
        self.tokens.iter().map(|token| match token.text.as_str() {
            "*" => {
                let star_replacement = match have_seen_star {
                    false => String::from("<span class=starred>"),
                    true => String::from("</span>"),
                };
                have_seen_star = !have_seen_star;
                star_replacement
            },
            _ => token.text.clone()
        }).join("")
    }
}

#[derive(Debug)]
struct AudioFile {
    file: PathBuf
}

impl AudioFile {
    fn build_note_field(&self) -> String {
        let end_file = self.file.file_name().unwrap().to_str().unwrap();
        format!("[sound:{end_file}]")
    }
}

#[derive(Debug, Deserialize)]
struct SimilarWord {
    word: String,
    translation: String
}

impl SimilarWord {
    fn build_string(&self, reading: &MandarinReading) -> String {
        let query_result = query_by_chinese(&self.word);
        let mut reading_str = String::from("");
        match reading {
            MandarinReading::Zhuyin => {
                if query_result[0].traditional.chars().count() == self.word.chars().count() {
                    reading_str.push_str(&query_result[0].derive_zhuyin());
                } else {
                    reading_str.push_str(&query_result.iter().map(|word| word.derive_zhuyin()).join(","));
                }
            },
            MandarinReading::Pinyin => {
                if query_result[0].traditional.chars().count() == self.word.chars().count() {
                    reading_str.push_str(&query_result[0].pinyin_marks);
                } else {
                    reading_str.push_str(&query_result.iter().map(|word| &word.pinyin_marks).join(" "));
                }
            },
        }
        
        let mut output = String::from(&self.word);
        output.push_str(", ");
        output.push_str(&reading_str);
        output.push_str(", ");
        output.push_str(&self.translation);
        output
    }
}

trait DeriveZhuyin {
    fn derive_zhuyin(&self) -> String;
}

impl DeriveZhuyin for WordEntry {
    fn derive_zhuyin(&self) -> String {
        return self.pinyin_numbers.split_whitespace()
            .map(|pinyin| encode_zhuyin(pinyin).or(Some(pinyin.to_string())).unwrap())
            .join(",");
    }
}

fn retry_policy() -> RetryPolicy {
    RetryPolicy::exponential(Duration::from_secs(1)).with_jitter(true).with_max_delay(Duration::from_secs(120))
}

fn init_deck(model_config: &ModelConfig) -> (Deck, Model, Model) {
    let deck = Deck::new(
        model_config.deck_id, 
        "Generated Mandarin Flashcards",
        "A Deck comprised of all the flashcards I have ever generated using my Script"
    );
    
    let word_model = Model::new(
        model_config.word_model_id, 
        "Mandarin Word",
        vec![
            Field::new("timestamp"),
            Field::new("Hanzi"),
            Field::new("Definition"),
            Field::new("Audio"),
            Field::new("Reading"),
            Field::new("Similar Words")
        ],
        vec![
            Template::new("Listening")
                .qfmt("Listen.{{Audio}}")
                .afmt(r#"
                    {{FrontSide}}
                    <hr id=answer>
                    {{Hanzi}}<br>{{Reading}}<br>{{Definition}}
                    <hr id=answer>
                    {{Similar Words}}
                "#),
            Template::new("Reading")
                .qfmt("{{Hanzi}}")
                .afmt(r#"
                    {{FrontSide}}
                    <hr id=answer>
                    {{Reading}}<br>{{Definition}}<br>{{Audio}}
                    <hr id=answer>
                    {{Similar Words}}
                "#)
        ]).css("
            .card {
                font-family: arial;
                font-size: 20px;
                text-align: center;
                color: black;
                background-color: white;
            }
        ");
    
    let sentence_model = Model::new(
        model_config.sentence_model_id,
        "Mandarin Sentence",
        vec![
                Field::new("timestamp"),
                Field::new("Hanzi"),
                Field::new("Meaning"),
                Field::new("Audio"),
                Field::new("Reading"),
            ],
        vec![
                Template::new("Listening")
                    .qfmt("Listen.{{Audio}}")
                    .afmt(r#"
                        {{FrontSide}}
                        <hr id=answer>
                        {{Hanzi}}<br>{{Reading}}<br>{{Meaning}}
                    "#),
                Template::new("Reading")
                    .qfmt("{{Hanzi}}")
                    .afmt(r#"
                        {{FrontSide}}
                        <hr id=answer>
                        {{Reading}}<br>{{Meaning}}<br>{{Audio}}
                    "#)
            ]).css("
                .card {
                    font-family: arial;
                    font-size: 20px;
                    text-align: center;
                    color: black;
                    background-color: white;
                }
    
                .starred {
                    color: red;
                }
            ");
    (deck, word_model, sentence_model)
}

fn tokenise_sentence(original_sentence: &str) -> Vec<Token> {
    let tokens = tokenize(original_sentence);
    let mut token_at_index: Vec<Token> = Vec::new();
    let mut current_index = 0;
    for token in tokens {
        let index_of_token = original_sentence[current_index..].find(token).unwrap() + current_index;
        if index_of_token > current_index {
            for non_mandarin_char in original_sentence[current_index..index_of_token].chars() {
                let non_mandarin_token = Token { text: non_mandarin_char.to_string(), word_entry: Option::None};
                token_at_index.push(non_mandarin_token);
            }
            current_index = index_of_token;
        }
        let word_entry = query_by_chinese(token);
        let value = Token { text: token.to_string(), word_entry: Option::Some(word_entry)};
        token_at_index.push(value);
        current_index += token.len()
    }
    if current_index < original_sentence.len() {
        for non_mandarin_char in original_sentence[current_index..original_sentence.len()].chars() {
            let non_mandarin_token = Token { text: non_mandarin_char.to_string(), word_entry: Option::None};
            token_at_index.push(non_mandarin_token);
        }
    }
    return token_at_index;
}

async fn _get_available_voices(client: &Client) {
    let res = client.get("https://uksouth.tts.speech.microsoft.com/cognitiveservices/voices/list")
        .header("Ocp-Apim-Subscription-Key", "909e875a50d34797bb5be7e8f86c2c4d")
        .send()
        .await.unwrap();

    let json = res.json::<Value>().await.unwrap();

    for voice in json.as_array().unwrap() {
        if voice["Locale"].eq("zh-TW") {
            println!("{:#?}", voice);
        }
    }
}

async fn get_tts(text: &str, tempdir: PathBuf, client: &Client, azure_config: &AzureConfig) -> AudioFile {
    let res = retry_policy().retry(||
        client.post(format!("https://{}.tts.speech.microsoft.com/cognitiveservices/v1", &azure_config.region))
            .header("Ocp-Apim-Subscription-Key", &azure_config.speech.key)
            .header("Content-Type", "application/ssml+xml")
            .header("X-Microsoft-OutputFormat", "audio-48khz-192kbitrate-mono-mp3")
            .header("User-Agent", "Rust Reqwest")
            .body(format!("
            <speak version='1.0' xml:lang='{0}'>
                <voice xml:lang='{0}' name='{1}'>
                    {2}
                </voice>
            </speak>", &azure_config.speech.locale, &azure_config.speech.voice_name, text))
            .send()
            .map(|res| res.unwrap().error_for_status())
        )
        .await.unwrap();
    trace!("Response from TTS: {:#?}", res);

    let bytes = res.bytes().await.unwrap();

    let encoded_text = url_escape::encode_component(text);
    let salt = Alphanumeric.sample_string(&mut rand::thread_rng(), 5);
    let file_destination = tempdir.join(format!("{:-<10.10}{}.mp3", encoded_text, salt));
    debug!("Audio Temp File: {}", file_destination.display());

    let mut file = File::create(&file_destination).unwrap();
    file.write_all(&bytes).unwrap();

    AudioFile {
        file: file_destination
    }
}

async fn get_translation(mandarin_text: &str, client: &Client, azure_config: &AzureConfig) -> String {
    let res = retry_policy().retry(||
        client.post("https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&to=en")
            .header("Ocp-Apim-Subscription-Key", &azure_config.translator.key)
            .header("Ocp-Apim-Subscription-Region", &azure_config.region)
            .header("Content-Type", "application/json; charset=UTF-8")
            .json(&json!([{"text": mandarin_text}]))
            .send()
            .map(|res| res.unwrap().error_for_status())
        )
        .await.unwrap();
    trace!("Translation Response: {:#?}", res);
    
    let json = res.json::<Value>().await.unwrap();
    let english_text = json[0]["translations"][0]["text"].as_str().unwrap();
    debug!("English Text from Translation: {}", english_text);
    english_text.to_string()
}

async fn get_transliteration(mandarin_text: &str, client: &Client, genanki_config: &GenankiConfig) -> (String, String) {
    let res = retry_policy().retry(||
        client.post(format!("https://api.cognitive.microsofttranslator.com/transliterate?api-version=3.0&language={}&fromScript={}&toScript=Latn", &genanki_config.mandarin.script.build_language(), &genanki_config.mandarin.script.build_from_script()))
            .header("Ocp-Apim-Subscription-Key", &genanki_config.azure.translator.key)
            .header("Ocp-Apim-Subscription-Region", &genanki_config.azure.region)
            .header("Content-Type", "application/json; charset=UTF-8")
            .json(&json!([{"text": mandarin_text}]))
            .send()
            .map(|res| res.unwrap().error_for_status())
        )
        .await.unwrap();
    trace!("Transliteration Response: {:#?}", res);
    
    let json = res.json::<Value>().await.unwrap();
    debug!("Json From Transliteration: {:#?}", json);

    let pinyin_reading = json[0]["text"].as_str().unwrap().to_owned();
    debug!("Pinyin Reading from Transliteration: {}", pinyin_reading);
    
    let zhuyin_reading = convert_pinyin_to_zhuyin(&pinyin_reading);

    match zhuyin_reading {
        Ok(zhuyin_reading) => {
            debug!("Zhuyin Reading from Pinyin: {}", zhuyin_reading);
        
            (pinyin_reading, zhuyin_reading)
        },
        Err(..) => {
            let mut rl = rustyline::DefaultEditor::new().unwrap();
            let line = rl.readline_with_initial ("Error in parsing pinyin, probably due to a word ending in u without being followed by an apostrophe. Please attempt a fix:", (&pinyin_reading, "")).unwrap();
            let zhuyin_reading = convert_pinyin_to_zhuyin(&line);
            (pinyin_reading, zhuyin_reading.unwrap())
        }
    }
        
}

fn convert_pinyin_to_zhuyin(pinyin_reading: &String) -> Result<String, Box<dyn Any + Send>> {
    let pinyin_parser = PinyinParser::new()
        .preserve_punctuations(true)
        .preserve_miscellaneous(true);
    let zhuyin_reading = panic::catch_unwind(|| {
        pinyin_parser.parse(&pinyin_reading.replace(" ", ",").replace("，,", "，"))
        .map(|pinyin_token| pinyin_zhuyin::pinyin_to_zhuyin(&pinyin_token).or(Some(pinyin_token)).unwrap())
        .collect::<String>()
    });
    zhuyin_reading
}

fn build_note_reading(reading: &str) -> String {
    let mut have_seen_star = false;
    reading.chars().map(|char| match char {
        '*' => {
            let star_replacement = match have_seen_star {
                false => String::from("<span class=starred>"),
                true => String::from("</span>")
            };
            have_seen_star = !have_seen_star;
            star_replacement
        }
        _ => char.to_string()
    }).collect::<String>()
}

async fn get_available_transliteration_scripts(client: &Client) {
    let res = client.get("https://api.cognitive.microsofttranslator.com/languages?api-version=3.0&scope=transliteration")
        .send()
        .await
        .unwrap();

    let json = res.json::<Value>().await.unwrap();
    println!("{:#?}", json["transliteration"]["zh-Hant"]);
}

async fn get_similar_words(word: &str, client: &Client, genanki_config: &GenankiConfig) -> Vec<SimilarWord> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_str("application/json").unwrap());
    headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", genanki_config.openai.key)).unwrap());
    if genanki_config.openai.organisation.is_some() {
        headers.insert(HeaderName::from_lowercase(b"openai-organization").unwrap(), HeaderValue::from_str(genanki_config.openai.organisation.as_ref().unwrap()).unwrap());
    }

    let res = retry_policy().retry(||
        client.post("https://api.openai.com/v1/chat/completions")
            .headers(headers.clone())
            .json(&json!({
                "model": "gpt-3.5-turbo",
                "messages": [
                    {
                        "role": "system",
                        "content": "You are a Taiwanese Mandarin Study Assistant generating study material"
                    },
                    {
                        "role": "user",
                        "content": format!("Generate 5 words closely related to {} which are used commonly in Taiwanese Mandarin.
                                            You should provide the words in {} and the English Translation in CSV format with two columns.",
                                        word, genanki_config.mandarin.script)
                    }
                ]
            }))
            .send()
            .map(|res| res.unwrap().error_for_status())
        )
        .await.unwrap();
    trace!("OpenAI Response: {:#?}", res);

    let json = res.json::<Value>().await.unwrap();
    debug!("Json From OpenAI: {:#?}", json);

    let message = json["choices"][0]["message"]["content"].as_str().unwrap();

    let rows = message.split("\n").map(|row| row.split(",").collect_vec()).collect_vec();

    let mut similar_words: Vec<SimilarWord> = Vec::new();

    for row in rows {
        if row.len() >= 2 && classify(&row[0]) == ClassificationResult::ZH { //Rows with actual csv content
            let similar_word = SimilarWord { word: row[0].trim().to_string(), translation: row[1].trim().to_string() };
            similar_words.push(similar_word);
        }
    }
    debug!("Similar Words Parsed: {:#?}", similar_words);

    similar_words
}

async fn process_word(word_model: Model, token: &Token, definition: Option<String>, tempdir: PathBuf) -> Option<(Note, AudioFile)> {
    //Exit prematurely if the word is not Mandarin
    match &token.word_entry {
        Some(word_entry) => {
            if word_entry.len() == 0 {
                warn!("Word wasn't recognisably Mandarin");
                return None
            }
        },
        None => {
            warn!("Word wasn't recognisable Mandarin");
            return None
        },
    };

    let config = CONFIG.get().unwrap();
    
    let client = reqwest::Client::new();

    let definition = match definition {
        Some(definition) => definition.to_owned(),
        None => match token.build_definition() {
            Some(definition) => definition,
            None => get_translation(&token.text, &client, &config.azure).await,
        },
    };
    debug!("Built Word Definition: {}", definition);
    let audio = get_tts(&token.text, tempdir, &client, &config.azure).await;
    let similar_words = get_similar_words(&token.text, &client, &config).await;
    let similar_words_string = similar_words.into_iter().map(|word| word.build_string(&config.mandarin.reading)).join("<br>");
    debug!("Built Similar Words for Note: {:#?}", similar_words_string);

    let word_note = build_word_note(word_model, token, definition, &audio, similar_words_string);
    debug!("Built Word Note");

    Some((word_note, audio))
}

fn build_word_note(word_model: Model, token: &Token, definition: String, audio: &AudioFile, similar_words_string: String) -> Note {
    let epoch_nanos_string = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_string();
    let word_note = Note::new(word_model, vec![
        &epoch_nanos_string,
        &token.text,
        &definition,
        &audio.build_note_field(),
        &token.build_reading_allow_multiple().unwrap(),
        &similar_words_string
    ]).unwrap();
    word_note
}

async fn process_sentence(sentence_model: Model, sentence: &MandarinSentence, definition: Option<String>, tempdir: PathBuf) -> Option<(Note, AudioFile)> {
    //Exit prematurely if none of the sentence is mandarin
    if !sentence.tokens.iter().any(|token| token.word_entry.as_ref().is_some_and(|word_entry| word_entry.len() > 0)) {
        warn!("Sentence had no recognisable Mandarin characters");
        return None;
    }

    let config = CONFIG.get().unwrap();

    let client = reqwest::Client::new();

    let plain_sentence = sentence.build_plain_sentence();
    debug!("Built Plain Sentence: {}", plain_sentence);

    let note_sentence = sentence.build_note_sentence();
    debug!("Built Sentence for Note: {}", note_sentence);
    let definition = match definition {
        Some(definition) => definition.to_owned(),
        None => get_translation(&plain_sentence, &client, &config.azure).await
    };
    debug!("Built Definition: {}", definition);
    let (pinyin_reading, zhuyin_reading) = get_transliteration(&sentence.raw_sentence, &client, &config).await;
    let note_reading = match &config.mandarin.reading {
        MandarinReading::Zhuyin => build_note_reading(&zhuyin_reading),
        MandarinReading::Pinyin => build_note_reading(&pinyin_reading),
    };
    debug!("Built Reading for Note: {}", note_reading);
    let audio = get_tts(&plain_sentence, tempdir, &client, &config.azure).await;

    let sentence_note = build_sentence_note(sentence_model, note_sentence, definition, &audio, note_reading);
    debug!("Built Sentence Note");

    Some((sentence_note, audio))
}

fn build_sentence_note(sentence_model: Model, note_sentence: String, definition: String, audio: &AudioFile, note_reading: String) -> Note {
    let epoch_nanos_string = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_string();
    let sentence_note = Note::new(sentence_model, vec![
        &epoch_nanos_string,
        &note_sentence,
        &definition,
        &audio.build_note_field(),
        &note_reading
    ]).unwrap();
    sentence_note
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn Error>>{
    CombinedLogger::init(
        vec![
            TermLogger::new(LevelFilter::Info, simplelog::Config::default(), TerminalMode::Mixed, ColorChoice::Auto),
            WriteLogger::new(LevelFilter::Trace, simplelog::Config::default(), File::create("trace.log").unwrap()),
        ]
    ).unwrap();

    CONFIG.set(parse_config()).unwrap();

    let tempdir = tempfile::Builder::new().prefix("gen-mandarin-anki-rs").tempdir().unwrap();

    let (mut deck, word_model, sentence_model) = init_deck(&CONFIG.get().unwrap().model);

    let mut input_csv_reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .trim(csv::Trim::All)
        .from_path("input.csv")?;
    let mut media: Vec<AudioFile> = Vec::new();
    let mut handles = Vec::new();
    for row in input_csv_reader.records() {
        let row = row.unwrap();
        let hanzi = row.get(0).unwrap();
        let definition = row.get(1).map(|definition| definition.to_owned());
        let tokenised_sentence = tokenise_sentence(hanzi);
        match tokenised_sentence.len() {
            1 => { 
                info!("Found Word: {}", hanzi);
                let model_clone = word_model.clone();
                let tempdir_clone = tempdir.path().to_owned();
                                handles.push(tokio::spawn(async move {
                    process_word(model_clone, &tokenised_sentence[0], definition, tempdir_clone).await
                }));
            },
            2.. => {
                info!("Found Sentence: {}", hanzi);
                let model_clone = sentence_model.clone();
                let tempdir_clone = tempdir.path().to_owned();
                let tokenised_sentence = MandarinSentence { raw_sentence: hanzi.to_owned(), tokens: tokenised_sentence };
                                handles.push(tokio::spawn(async move {
                    process_sentence(model_clone, &tokenised_sentence, definition, tempdir_clone).await
                }));
            },
            _ => {},
        };
    }

    for option in join_all(handles).await {
        let option = option.unwrap();
        if option.is_some() {
            let (note, audio) = option.unwrap();
            deck.add_note(note);
            media.push(audio);
        }
    }

    let mut package = Package::new(vec![deck], media.iter().map(|path| path.file.to_str().unwrap()).collect_vec()).unwrap();
    package.write_to_file("output.apkg").unwrap();

    Ok(())
}

#[test]
fn test_parse_config() {
    let config = parse_config();
    println!("Parsed Config: {:#?}", config);
}

#[test]
fn test_derive_zhuyin() {
    let word = query_by_chinese("刮目");
    println!("Parsed Word: {:#?}", word);
    println!("Generated Sentence: {:#?}", word[0].derive_zhuyin());
}

#[test]
fn test_build_note_sentence() {
    let hanzi = String::from("你今天看起來很*時尚*");
    let tokens = tokenise_sentence(&hanzi);
    let sentence = MandarinSentence{raw_sentence: hanzi, tokens: tokens};
    let note_sentence = sentence.build_note_sentence();
    println!("Note sentence: {}", note_sentence);
    assert!(note_sentence.contains("</span>"))
}

#[test]
fn test_parse_csv() {
    let data = "\
    我的頭髮太厚了，我要打薄, \"My hair is too thick, I need to thin it out\"
    我朋友是個街友*基金會*的員工, My friend works at a homelessness charity
    基金會
    ";
    let mut input_csv_reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        // .trim(csv::Trim::All)
        .from_reader(data.as_bytes());
    let first_row = input_csv_reader.records().next().unwrap().unwrap();
    println!("First Row: {:?}", first_row);
    assert_eq!(first_row.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_get_available_voices() {
    let client = reqwest::Client::new();
    //Just run and check stdout
    _get_available_voices(&client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_get_tts() {
    let client = reqwest::Client::new();
    let tempdir = tempfile::Builder::new().prefix("test_synthesize_text").tempdir().unwrap();
    let audio_file = get_tts("你好", tempdir.into_path(), &client, &parse_config().azure).await;
    println!("Created Audio FIle: {:#?}", audio_file);
    assert!(audio_file.file.exists())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_get_available_transliteration_scripts() {
    let client = reqwest::Client::new();
    //Just run and check stdout
    get_available_transliteration_scripts(&client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_get_translation() {
    let client = reqwest::Client::new();
    let translation = get_translation("Hello", &client, &parse_config().azure).await;
    println!("Got Translation: {translation}");
    assert!(!translation.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_get_transliteration() {
    let client = reqwest::Client::new();
    let (pinyin_reading, zhuyin_reading) = get_transliteration("都是因為媽媽太*寵*他，才會這麼軟弱", &client, &parse_config()).await;
    println!("Got Pinyin: {pinyin_reading}, Zhuyin: {zhuyin_reading}");
    assert!(!pinyin_reading.is_empty());
    assert!(!zhuyin_reading.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_get_similar_word() {
    let client = reqwest::Client::new();
    let similar_words = get_similar_words("你好", &client, &parse_config()).await;
    println!("Got Similar Words: {:#?}", similar_words);
    assert!(similar_words.len() > 0);
}