//! The reply box's message history: Up and Down step through what John
//! said earlier in the highlighted agent's session, newest first, as in a
//! shell; stepping down past the newest brings back the draft.

/// Where the reply box is in its session's messages.
#[derive(Default)]
pub struct History {
    /// How far back the box is: 1 for the newest message, 0 while it holds
    /// the draft.
    back: usize,
    /// What was typed before Up first recalled a message.
    draft: String,
}

impl History {
    /// The message one further back, given the session's messages oldest
    /// first and the box's text now; None at the oldest.
    pub fn older(&mut self, messages: &[String], current: &str) -> Option<String> {
        if self.back >= messages.len() {
            return None;
        }
        if self.back == 0 {
            self.draft = current.to_string();
        }
        self.back += 1;
        Some(messages[messages.len() - self.back].clone())
    }

    /// The message one nearer now, or the draft after the newest; None
    /// while the box holds the draft.
    pub fn newer(&mut self, messages: &[String]) -> Option<String> {
        match self.back {
            0 => None,
            1 => {
                self.back = 0;
                Some(std::mem::take(&mut self.draft))
            }
            _ => {
                self.back -= 1;
                let i = messages.len().checked_sub(self.back);
                if let Some(m) = i.and_then(|i| messages.get(i)) {
                    return Some(m.clone());
                }
                // The transcript shrank under us: back to the draft.
                self.back = 0;
                Some(std::mem::take(&mut self.draft))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(m: &[&str]) -> Vec<String> {
        m.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn up_goes_back_newest_first_and_down_returns_to_the_draft() {
        let m = msgs(&["first", "second", "third"]);
        let mut h = History::default();
        assert_eq!(h.newer(&m), None);
        assert_eq!(h.older(&m, "draft").as_deref(), Some("third"));
        assert_eq!(h.older(&m, "third").as_deref(), Some("second"));
        assert_eq!(h.older(&m, "second").as_deref(), Some("first"));
        assert_eq!(h.older(&m, "first"), None);
        assert_eq!(h.newer(&m).as_deref(), Some("second"));
        assert_eq!(h.newer(&m).as_deref(), Some("third"));
        assert_eq!(h.newer(&m).as_deref(), Some("draft"));
        assert_eq!(h.newer(&m), None);
    }

    #[test]
    fn no_messages_leave_the_box_alone() {
        let mut h = History::default();
        assert_eq!(h.older(&[], "draft"), None);
        assert_eq!(h.newer(&[]), None);
    }
}
