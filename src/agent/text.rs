pub(super) fn collapse_ws(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_space = false;

    for ch in input.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_and_trims_whitespace_without_an_intermediate_vec() {
        assert_eq!(collapse_ws("  alpha\tbeta\n gamma  "), "alpha beta gamma");
        assert_eq!(collapse_ws("\u{2003}alpha\u{00a0}beta"), "alpha beta");
        assert_eq!(collapse_ws("   "), "");
    }
}
