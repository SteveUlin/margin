//! Who the person is, as far as margin's data and its messages go.
//!
//! Every decision the person makes in the pane is stored with `by` = `HUMAN` ("you", the word the
//! decided rows already use). State written before that says "sulin" (margin's first user), so
//! `is_human` accepts both. What Claude reads names the person by `user_name` from the config
//! (default "the user"): the messages are written with the word "sulin" and `say` swaps in the
//! configured name on the way out, so no one's name is baked into the source's behaviour.

/// The `by` of the person's decisions and events.
pub const HUMAN: &str = "you";

/// Is this `by` the person's? ("sulin" is the identity state used before `HUMAN`.)
pub fn is_human(by: &str) -> bool {
    by == HUMAN || by == "sulin"
}

/// The person's name in messages to Claude: the config's `user_name`, else "the user".
pub fn name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let n = crate::config::Config::load().user_name;
        if n.trim().is_empty() { "the user".to_string() } else { n }
    })
}

/// A message to Claude with the person's configured name in it.
pub fn say(text: &str) -> String {
    if name() == "sulin" {
        return text.to_string();
    }
    text.replace("sulin's", &format!("{}'s", name())).replace("sulin", name())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_state_still_counts_as_the_person() {
        assert!(is_human("you") && is_human("sulin") && !is_human("claude"));
    }
}
