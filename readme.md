# Gen-Mandarin-Anki-rs
This is my Rust script to generate Mandarin Anki flashcards using Microsoft Azure (translation, transliteration and text-to-speech), and OpenAI(ChatGPT).  
I find this tool to be incredibly helpful in my Mandarin study. Whenever I encounter a new word or phrase that I want to remember (whether while reading, watching stuff, speaking with Taiwanese people, etc...) I will write it down quickly in my Google Keep Notes. Preferably I will also write down the sentence that the word came in, or if not I will usually write my own example sentence and have my Mandarin tutor check it before I create the flashcard.  
A few times a week I will take my list of sentences and copy them into `input.csv` and then run the script. This way I always have new flashcards to study from which are relevant to what I have been doing recently.
## Usage
There are two things you need to do before you can use the script for the first time.  
1. Copy the `example_config.yml` file in the root directory and create a file just called `config.yml`. You will then need to fill in the various pieces of config with their correct values.  
    - The `example_config.yml` file includes links to tutorial pages for Azure and OpenAI for setting up your account. You won't need to follow the whole tutorial, just go far enough to have provisioned the correct Azure/OpenAI services and got the keys you need.  
    - The example config contains ids for the word_model, sentence_model and deck. These are there to ensure that when you use the script a second time the cards you import will join the same deck as the previous import rather than creating a new one. You can feel free to change these, they're just the randomly generated numbers I use, but if you do change them make sure not to change them again or else you'll end up with multiple separate decks.
2. Create a file in the root directory called `input.csv`. This is where you will write the words and sentences that you want to translate, in CSV format.  

Now that you have set everything up correctly, just run the rust binary and it will create a file in the root directory called `output.apkg`.
- `cargo run --release`  

Now, open the Anki app on your Mac/PC and select `file/import` and point it to the `output.apkg` file.  
Any errors should be printed to the terminal as the script is running, but running the binary will also have created a `trace.log` file which has much more verbose logging. If there are any errors with your connection to any of the APIs you should be able to tell from there what happened.
## Input Format
A small example of a typical `input.csv` file looks like this:
![example input file](/images/example_input.png)
On the first line I have entered a Mandarin sentence, followed by the English translation. In the Mandarin sentence I have surrounded the word I am most interested in with \*stars\*, which the script will interpret and will highlight that word and the accompanying reading in the final flashcard.  
You don't have to use stars to highlight words, and you don't have to include an English translation. If the script can't find a Mandarin translation it will use Microsoft Azure to generate one, but I genenrally think making one myself is better practice.  
The second line is just a single word. When using a Mandarin dictionary to tokenise the sentence, if the script finds that a line only has a single word then it treats it differently, using ChatGPT to generate a list of related words. Since ChatGPT is more an art than a science, this list isn't always guaranteed to be formatted properly, or to adhere to your preferences regarding Simplified/Traditional characters, but I find it works great 9 times out of 10.
This input file produced an `output.apkg` which I imported into my Anki containing the following two cards:
![example sentence output](/images/example_sentence_output.png)
A sentence card, with text to speech audio and the starred hanzi and zhuyin highlighted.
![example word output](/images/example_word_output.png)
A word card, with text to speech audio, the dictionary definition of the word, and a list of ChatGPT generated related words.  
This list of related words isn't great. They do range in usefulness, but they cost practically nothing to add and they often are really helpful. Running the script again produced these words:
`   {
        word: "平反",
        translation: "Exoneration",
    },
    {
        word: "悔過",
        translation: "Humble repentance",
    },
    {
        word: "肅清",
        translation: "Cleanse",
    },
    {
        word: "改革",
        translation: "Reform",
    },
    {
        word: "謝罪",
        translation: "Apology",
    }`  
Both cards have a reading version and a listening version. The reading version initially only shows the hanzi, and the listening version initially only plays the audio. They both share the same reverse.