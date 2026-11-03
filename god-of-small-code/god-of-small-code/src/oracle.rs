//! The Oracle of the God of Small Code.
//!
//! Contract: every answer is cryptic, confident and completely useless.
//! The only thing borrowed from the prayer is one harmless word, so the
//! god *appears* to have listened. Nothing here is ever actionable advice.

/// Tiny xorshift64* generator. No dependencies, good enough for omens.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // splitmix64 scramble so that nearby seeds give unrelated streams
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Rng(if z == 0 { 0x1234_5678_9ABC_DEF1 } else { z })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / 16_777_216.0
    }

    /// Uniform in 0..n (n must be > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const OPENERS: &[&str] = &[
    "The flame flickers twice, which means nothing.",
    "A semicolon falls somewhere in the dark.",
    "The god of small code looks up from a very long one-liner.",
    "He sighs in the voice of a cron job.",
    "The embers spell something, then think better of it.",
    "A distant printer jams in sympathy.",
    "The god cracks his knuckles. Thirty-one tabs are open in his mind.",
    "From the smoke, a voice, slightly buffered:",
    "The flame turns toward you, then toward a wall.",
    "Somewhere a pipe breaks. Possibly a metaphor.",
    "He consults the scroll of man pages. It consults him back.",
    "The ash shifts. Something has been decided, and it is not your concern.",
    "The god pauses, like a script waiting on stdin.",
    "Thunder, but only in a log file.",
    "He hums the note of a laptop fan under load.",
    "A single byte detaches from the altar and leaves without explanation.",
    "The god squints at your prayer as at a stack trace in a foreign language.",
    "The flame bows, which is how fire shrugs.",
];

/// `{w}` is replaced by a harmless word taken from the prayer.
const OMENS: &[&str] = &[
    "The answer lies in the third tab, which you closed on Tuesday.",
    "Thou seekest {w}. Know that {w} is merely a variable awaiting a better name.",
    "What you ask is written in a language that only compiles on Thursdays.",
    "Seven whitespace characters know your answer. None will speak.",
    "Pipe it through something larger.",
    "The truth is hidden in a config file that does not exist yet.",
    "It is not the bug. It is also not not the bug.",
    "The answer is 'it depends', and it depends on a moon you cannot see.",
    "Rename the thing. Then rename it back. Then weep gently.",
    "Thou shalt find it behind the fourth backslash.",
    "A man once asked about {w} too. He is still in the scrollback.",
    "The answer is forty-two, in a base that has not been invented.",
    "Seek the line you were certain was fine.",
    "The answer is a tab, the question is a space, and the god is tired.",
    "Beware the number that is almost right.",
    "That which you call {w} is only an exit code in disguise.",
    "Ask again after the build. It will not have finished.",
    "It was the clock. It is always the clock. (It was not the clock.)",
    "Delete nothing. Add nothing. Stand near it.",
    "Thy question hath been forwarded to a mailing list from 2011.",
    "The solution is shaped like a regex and bites like one.",
    "Count the quotation marks. Count them again. They have changed.",
    "Within {w} there is a smaller {w}, and within that, a lunch break.",
    "The omens say: yes, no, and 'works on my machine'.",
    "Thy answer lives in a file named final_FINAL2_really.txt.",
    "I see a loop. It has seen you also.",
    "Blame the encoding. The encoding is guilty of something, at least.",
    "Wait for the progress bar. It is a liar, but a punctual one.",
    "Thou must first become a smaller script.",
    "The answer is in the logs. The logs are in the answer.",
    "Thy prayer hath been logged, and the log hath been rotated.",
    "The cache remembers. The cache is wrong.",
    "Somebody has touched the timezone.",
    "{w}, you say. The god nods slowly, as one nods at an elevator.",
    "A comma lies at the root of this, and it knows what it did.",
    "Make it smaller. Smaller than that. Now it is a poem.",
    "Thou art one off-by-one from enlightenment, in either direction.",
    "Read the error message as a haiku, and learn nothing, beautifully.",
    "Turn it off and on again. Not the machine. The concept.",
    "The truth is a hash collision waiting to be noticed.",
    "{w} is a door. The door is a rumour. The rumour is on fire.",
    "Thou shalt know the answer when the build goes green, and not one minute before, and not after either.",
    "Somewhere, a script exits 0 and feels nothing.",
    "The answer is cached in a place you have only visited in dreams.",
];

const CLOSERS: &[&str] = &[
    "Go in peace, and chmod nothing.",
    "This is all I have, and it is not much, and it is yours.",
    "Offer more files. The flame is still thinking about the last one.",
    "The god returns to his one-liner.",
    "Remember: small is beautiful, and also faster to delete.",
    "Now go. Hydrate. Commit something tiny.",
    "The flame resumes burning, with the air of having said enough.",
    "He vanishes into /dev/null, which he finds restful.",
    "(This answer is guaranteed useless. The god has a perfect record.)",
    "Let there be fewer dependencies.",
    "May thy diffs be short and thy rebases forgiving.",
    "May thy tabs be fewer than thy purposes.",
    "The matter is closed. Reopened. Closed as duplicate.",
    "Amen, or Enter, whichever comes first.",
    "Go forth and ship something the size of a postcard.",
    "He asks that you not mention this to the linter.",
];

/// (keyword in the prayer, special answer). First match wins.
const SPECIALS: &[(&str, &str)] = &[
    ("regex", "Regular expressions are the god's least favourite child. He loves it anyway, the way one loves a cat that has bitten someone."),
    ("python", "Whitespace is sacred. He has counted yours. He will not tell you the number."),
    ("rust", "The god nods slowly. 'Borrowed,' he says. 'Never owned.' This applies to all things, including this shrine."),
    ("powershell", "He regards your quoting with the quiet horror of a man holding the wrong end of a parenthesis."),
    ("bash", "He regards your quoting with the quiet horror of a man holding the wrong end of a parenthesis."),
    ("sudo", "No."),
    ("bug", "The bug is already aware of you. It was here first, and it brought snacks."),
    ("error", "The error is not in your code. It is in the space between your code and your hopes."),
    ("why", "'Why' is a question for the larger gods. This one only handles the small ones, and only on weekdays."),
    ("meaning", "Forty-two lines. Fewer, if you pipe it."),
    ("git", "The god does not speak of git. He merely rebases, silently, by torchlight."),
    ("help", "The god is helping. He is helping in a direction you have not yet noticed, at a speed you cannot measure."),
];

const SILENCE: &str =
    "Thou prayest in silence. The god approves: silence is the only language with no breaking changes.";

/// Pull one harmless word out of the prayer (letters/digits only).
fn pick_word(prayer: &str) -> String {
    let mut best = String::new();
    for tok in prayer.split(|c: char| !c.is_alphanumeric()) {
        let n = tok.chars().count();
        if (3..=18).contains(&n) && n > best.chars().count() {
            best = tok.to_lowercase();
        }
    }
    if best.is_empty() {
        "the thing".to_string()
    } else {
        best
    }
}

fn hash(s: &str) -> u64 {
    // FNV-1a
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// The god's reply. `seed` should vary per call (time is fine).
pub fn answer(prayer: &str, seed: u64) -> String {
    let prayer = prayer.trim();
    if prayer.is_empty() {
        return SILENCE.to_string();
    }
    let mut rng = Rng::new(seed ^ hash(prayer));
    let lower = prayer.to_lowercase();

    if let Some((_, text)) = SPECIALS.iter().find(|(k, _)| lower.contains(k)) {
        if rng.f32() < 0.6 {
            return (*text).to_string();
        }
    }

    let w = pick_word(prayer);
    let omen = rng.pick(OMENS).replace("{w}", &w);
    let shape = rng.f32();
    if shape < 0.55 {
        format!("{} {} {}", rng.pick(OPENERS), omen, rng.pick(CLOSERS))
    } else if shape < 0.80 {
        format!("{} {}", omen, rng.pick(CLOSERS))
    } else {
        format!("{} {}", rng.pick(OPENERS), omen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_sane() {
        for i in 0..5000u64 {
            let a = answer("why does my script segfault on tuesdays?", i);
            assert!(!a.is_empty() && a.len() < 600, "{a}");
            assert!(!a.contains('{') && !a.contains('}'), "{a}");
        }
    }

    #[test]
    fn silence_and_odd_input() {
        assert_eq!(answer("   ", 1), SILENCE);
        for p in ["??", "💥💥", "a", "<script>alert(1)</script>", "{w}"] {
            let a = answer(p, 7);
            assert!(!a.contains("{w}"), "{a}");
        }
    }

    #[test]
    fn variety() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..400u64 {
            seen.insert(answer("tell me about sockets", i));
        }
        assert!(seen.len() > 300, "only {} distinct", seen.len());
    }

    #[test]
    fn word_is_harmless() {
        assert_eq!(pick_word("what is a <b>kubernetes</b> pod"), "kubernetes");
        assert_eq!(pick_word("??"), "the thing");
    }
}
