//! One file per command group. A group exposes `Cmd` (clap) and `run(ctx, cmd)`; `cli.rs` only wires them.
pub mod auth;
pub mod device;
pub mod io;
pub mod pp;
pub mod raw;
pub mod rule;
pub mod sync;

/// Case-insensitive wildcard match of the whole text: `*` is any run of characters, `?` is one character.
/// Both `device list --model` and `io values --model` use it.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None; // (pattern index after the last `*`, text index it covers)
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            backtrack = Some((pi + 1, ti));
            pi += 1;
        } else if let Some((star_pi, star_ti)) = backtrack {
            backtrack = Some((star_pi, star_ti + 1));
            pi = star_pi;
            ti = star_ti + 1;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn glob_is_case_insensitive_with_star_and_question_mark() {
        assert!(glob_match("Virtual*", "VirtualSwitch"));
        assert!(glob_match("*switch", "VirtualSwitch"));
        assert!(glob_match("v*t*h", "VirtualSwitch"));
        assert!(glob_match("*", ""));
        assert!(glob_match("virtualswitch", "VirtualSwitch"));
        assert!(glob_match("Virtual?witch", "VirtualSwitch"));
        assert!(!glob_match("Virtual?", "VirtualSwitch"));
        assert!(!glob_match("Virtual", "VirtualSwitch"));
        assert!(!glob_match("a*ab", "ab"));
        assert!(!glob_match("*x*", "VirtualSwitch"));
        assert!(!glob_match("?", ""));
    }
}
