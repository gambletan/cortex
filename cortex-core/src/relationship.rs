//! Relationship inference — auto-extract relationships between people from text.
//!
//! Pattern-based extraction (no LLM) for detecting interpersonal relationships
//! mentioned in conversations. Supports English and Chinese.

/// An inferred relationship between two entities.
#[derive(Debug, Clone)]
pub struct InferredRelationship {
    pub person_a: String,
    pub person_b: String,
    pub relation: String,
    pub confidence: f32,
}

/// Extract relationships from text.
pub fn extract_relationships(text: &str) -> Vec<InferredRelationship> {
    let mut results = Vec::new();
    extract_english_relationships(text, &mut results);
    extract_chinese_relationships(text, &mut results);
    results
}

/// Get the inverse of a relationship type, if one exists.
/// Symmetric relations (friend_of, colleague_of, etc.) map to themselves.
/// Asymmetric relations map to their counterpart (manages ↔ reports_to).
pub fn inverse_relation(relation: &str) -> Option<&'static str> {
    match relation {
        // Symmetric — inverse is the same
        "colleague_of" => Some("colleague_of"),
        "friend_of" => Some("friend_of"),
        "spouse_of" => Some("spouse_of"),
        "sibling_of" => Some("sibling_of"),
        "partner_of" => Some("partner_of"),
        "roommate_of" => Some("roommate_of"),
        "classmate_of" => Some("classmate_of"),
        "teammate_of" => Some("teammate_of"),
        "neighbor_of" => Some("neighbor_of"),
        "lives_with" => Some("lives_with"),
        "dating" => Some("dating"),
        // Asymmetric — inverse is the counterpart
        "manages" => Some("reports_to"),
        "reports_to" => Some("manages"),
        "mentors" => Some("mentored_by"),
        "mentored_by" => Some("mentors"),
        "teaches" => Some("taught_by"),
        "taught_by" => Some("teaches"),
        "parent_of" => Some("child_of"),
        "child_of" => Some("parent_of"),
        _ => None,
    }
}

/// Generate bidirectional relationships: for each relationship, also produce its inverse.
/// E.g., "Alice manages Bob" → also yields "Bob reports_to Alice".
pub fn with_inverses(relationships: &[InferredRelationship]) -> Vec<InferredRelationship> {
    let mut result = Vec::with_capacity(relationships.len() * 2);
    for rel in relationships {
        result.push(rel.clone());
        if let Some(inv) = inverse_relation(&rel.relation) {
            result.push(InferredRelationship {
                person_a: rel.person_b.clone(),
                person_b: rel.person_a.clone(),
                relation: inv.to_string(),
                confidence: rel.confidence,
            });
        }
    }
    result
}

// ── English patterns ─────────────────────────────────────────────────────────

fn extract_english_relationships(text: &str, results: &mut Vec<InferredRelationship>) {
    let lower = text.to_lowercase();

    // "X is Y's {relation}" patterns
    let possessive_relations = [
        ("boss", "reports_to"),
        ("manager", "reports_to"),
        ("supervisor", "reports_to"),
        ("mentor", "mentored_by"),
        ("teacher", "taught_by"),
        ("professor", "taught_by"),
        ("colleague", "colleague_of"),
        ("coworker", "colleague_of"),
        ("co-worker", "colleague_of"),
        ("friend", "friend_of"),
        ("partner", "partner_of"),
        ("wife", "spouse_of"),
        ("husband", "spouse_of"),
        ("spouse", "spouse_of"),
        ("brother", "sibling_of"),
        ("sister", "sibling_of"),
        ("sibling", "sibling_of"),
        ("father", "parent_of"),
        ("mother", "parent_of"),
        ("parent", "parent_of"),
        ("son", "child_of"),
        ("daughter", "child_of"),
        ("child", "child_of"),
        ("roommate", "roommate_of"),
        ("neighbor", "neighbor_of"),
        ("classmate", "classmate_of"),
    ];

    // Pattern: "{Name} is {Name}'s {relation}"
    for (rel_word, rel_type) in &possessive_relations {
        let pattern = format!("'s {}", rel_word);
        if let Some(pos) = lower.find(&pattern) {
            // Extract person B (before 's)
            let before = &text[..pos];
            if let Some(person_b) = extract_name_before(before) {
                // Extract person A (before "is")
                let prefix = &lower[..lower[..pos].rfind(&person_b.to_lowercase()).unwrap_or(0)];
                if let Some(is_pos) = prefix.rfind(" is ") {
                    let person_a_text = &text[..is_pos];
                    if let Some(person_a) = extract_name_end(person_a_text) {
                        results.push(InferredRelationship {
                            person_a: person_a.to_string(),
                            person_b: person_b.to_string(),
                            relation: rel_type.to_string(),
                            confidence: 0.85,
                        });
                    }
                }
            }
        }
    }

    // Pattern: "{Name} works with {Name}"
    let collab_patterns = [
        (" works with ", "colleague_of"),
        (" works for ", "reports_to"),
        (" reports to ", "reports_to"),
        (" manages ", "manages"),
        (" mentors ", "mentors"),
        (" teaches ", "teaches"),
        (" married to ", "spouse_of"),
        (" dating ", "dating"),
        (" lives with ", "lives_with"),
    ];

    for (pattern, rel_type) in &collab_patterns {
        if let Some(pos) = lower.find(pattern) {
            let before = &text[..pos];
            let after = &text[pos + pattern.len()..];
            if let (Some(person_a), Some(person_b)) =
                (extract_name_end(before), extract_name_start(after))
            {
                results.push(InferredRelationship {
                    person_a: person_a.to_string(),
                    person_b: person_b.to_string(),
                    relation: rel_type.to_string(),
                    confidence: 0.80,
                });
            }
        }
    }

    // Pattern: "{Name} and {Name} are {relation}s"
    let group_relations = [
        ("are colleagues", "colleague_of"),
        ("are friends", "friend_of"),
        ("are partners", "partner_of"),
        ("are siblings", "sibling_of"),
        ("are roommates", "roommate_of"),
        ("are classmates", "classmate_of"),
        ("are teammates", "teammate_of"),
    ];

    for (pattern, rel_type) in &group_relations {
        if let Some(pos) = lower.find(pattern) {
            let before = &text[..pos];
            if let Some(and_pos) = before.rfind(" and ") {
                let person_a_text = &before[..and_pos];
                let person_b_text = &before[and_pos + 5..];
                if let (Some(person_a), Some(person_b)) = (
                    extract_name_end(person_a_text),
                    extract_name_start(person_b_text.trim()),
                ) {
                    results.push(InferredRelationship {
                        person_a: person_a.to_string(),
                        person_b: person_b.to_string(),
                        relation: rel_type.to_string(),
                        confidence: 0.85,
                    });
                }
            }
        }
    }
}

// ── Chinese patterns ─────────────────────────────────────────────────────────

fn extract_chinese_relationships(text: &str, results: &mut Vec<InferredRelationship>) {
    // "A是B的{relation}"
    let zh_possessive = [
        ("老板", "reports_to"),
        ("上司", "reports_to"),
        ("经理", "reports_to"),
        ("领导", "reports_to"),
        ("老师", "taught_by"),
        ("导师", "mentored_by"),
        ("同事", "colleague_of"),
        ("朋友", "friend_of"),
        ("室友", "roommate_of"),
        ("同学", "classmate_of"),
        ("队友", "teammate_of"),
        ("妻子", "spouse_of"),
        ("丈夫", "spouse_of"),
        ("老公", "spouse_of"),
        ("老婆", "spouse_of"),
        ("哥哥", "sibling_of"),
        ("姐姐", "sibling_of"),
        ("弟弟", "sibling_of"),
        ("妹妹", "sibling_of"),
        ("爸爸", "parent_of"),
        ("妈妈", "parent_of"),
        ("父亲", "parent_of"),
        ("母亲", "parent_of"),
        ("儿子", "child_of"),
        ("女儿", "child_of"),
    ];

    for (rel_word, rel_type) in &zh_possessive {
        // Pattern: "A是B的{relation}"
        let pattern = format!("的{}", rel_word);
        if let Some(pos) = text.find(&pattern) {
            // Find "是" before the possessive
            let before = &text[..pos];
            if let Some(shi_pos) = before.rfind('是') {
                let person_a_end = &text[..shi_pos];
                let person_b_start = &text[shi_pos + '是'.len_utf8()..pos];
                let person_a = extract_zh_name_end(person_a_end);
                let person_b = person_b_start.trim();
                if !person_a.is_empty() && !person_b.is_empty() {
                    results.push(InferredRelationship {
                        person_a: person_a.to_string(),
                        person_b: person_b.to_string(),
                        relation: rel_type.to_string(),
                        confidence: 0.85,
                    });
                }
            }
        }
    }

    // "A和B是同事/朋友"
    let zh_group = [
        ("是同事", "colleague_of"),
        ("是朋友", "friend_of"),
        ("是同学", "classmate_of"),
        ("是室友", "roommate_of"),
        ("是队友", "teammate_of"),
    ];

    for (pattern, rel_type) in &zh_group {
        if let Some(pos) = text.find(pattern) {
            let before = &text[..pos];
            // Look for "和" or "与" connector
            for connector in &["和", "与", "跟"] {
                if let Some(conn_pos) = before.rfind(connector) {
                    let person_a = extract_zh_name_end(&before[..conn_pos]);
                    let person_b = &before[conn_pos + connector.len()..];
                    let person_b = person_b.trim();
                    if !person_a.is_empty() && !person_b.is_empty() {
                        results.push(InferredRelationship {
                            person_a: person_a.to_string(),
                            person_b: person_b.to_string(),
                            relation: rel_type.to_string(),
                            confidence: 0.85,
                        });
                    }
                }
            }
        }
    }
}

// ── Name extraction helpers ──────────────────────────────────────────────────

/// Extract a capitalized name from the end of a string.
fn extract_name_end(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Walk backwards collecting capitalized words
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let mut name_start = words.len();
    for i in (0..words.len()).rev() {
        let first_char = words[i].chars().next()?;
        if first_char.is_uppercase() {
            name_start = i;
        } else {
            break;
        }
    }

    if name_start < words.len() {
        let name = words[name_start..].join(" ");
        // Find the position in original text
        if let Some(pos) = trimmed.rfind(&name) {
            return Some(&trimmed[pos..pos + name.len()]);
        }
    }
    None
}

/// Extract a capitalized name from the start of a string.
fn extract_name_start(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let mut name_end = 0;
    for word in &words {
        let first_char = word.chars().next()?;
        if first_char.is_uppercase() {
            name_end += 1;
        } else {
            break;
        }
    }

    if name_end > 0 {
        let name = words[..name_end].join(" ");
        if let Some(pos) = trimmed.find(&name) {
            return Some(&trimmed[pos..pos + name.len()]);
        }
    }
    None
}

/// Extract a name before a position in Chinese text.
fn extract_name_before(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Take last few chars as name (Chinese names are typically 2-4 chars)
    let chars: Vec<char> = trimmed.chars().collect();
    let start = if chars.len() > 4 { chars.len() - 4 } else { 0 };
    // Find where the name starts (look for non-name characters)
    let mut name_start = start;
    for i in (start..chars.len()).rev() {
        if chars[i].is_whitespace() || "，。！？、；：\u{201c}\u{201d}\u{2018}\u{2019}".contains(chars[i]) {
            name_start = i + 1;
            break;
        }
    }
    if name_start < chars.len() {
        let name: String = chars[name_start..].iter().collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Extract a Chinese name from the end of text.
fn extract_zh_name_end(text: &str) -> &str {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.is_empty() {
        return "";
    }
    // Walk backwards, stop at punctuation/whitespace
    let mut start = chars.len();
    let zh_stops: &[char] = &['，', '。', '！', '？', '、', '；', '：', '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}', '的', '是'];
    for i in (0..chars.len()).rev() {
        if chars[i].is_whitespace() || zh_stops.contains(&chars[i]) {
            break;
        }
        start = i;
    }
    let byte_start: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
    &trimmed[byte_start..]
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_works_with() {
        let rels = extract_relationships("Alice works with Bob on the project");
        assert!(!rels.is_empty());
        assert_eq!(rels[0].person_a, "Alice");
        assert_eq!(rels[0].person_b, "Bob");
        assert_eq!(rels[0].relation, "colleague_of");
    }

    #[test]
    fn test_reports_to() {
        let rels = extract_relationships("Alice reports to Bob");
        assert!(!rels.is_empty());
        assert_eq!(rels[0].relation, "reports_to");
    }

    #[test]
    fn test_are_friends() {
        let rels = extract_relationships("Alice and Bob are friends");
        assert!(!rels.is_empty());
        assert_eq!(rels[0].relation, "friend_of");
    }

    #[test]
    fn test_zh_colleague() {
        let rels = extract_relationships("Alice和Bob是同事");
        assert!(!rels.is_empty());
        assert_eq!(rels[0].relation, "colleague_of");
    }

    #[test]
    fn test_zh_possessive() {
        let rels = extract_relationships("Alice是Bob的老板");
        assert!(!rels.is_empty());
        assert_eq!(rels[0].person_b, "Bob");
        assert_eq!(rels[0].relation, "reports_to");
    }

    #[test]
    fn test_no_relationship() {
        let rels = extract_relationships("The weather is nice today");
        assert!(rels.is_empty());
    }

    #[test]
    fn test_inverse_symmetric() {
        assert_eq!(inverse_relation("friend_of"), Some("friend_of"));
        assert_eq!(inverse_relation("colleague_of"), Some("colleague_of"));
        assert_eq!(inverse_relation("spouse_of"), Some("spouse_of"));
    }

    #[test]
    fn test_inverse_asymmetric() {
        assert_eq!(inverse_relation("manages"), Some("reports_to"));
        assert_eq!(inverse_relation("reports_to"), Some("manages"));
        assert_eq!(inverse_relation("parent_of"), Some("child_of"));
        assert_eq!(inverse_relation("child_of"), Some("parent_of"));
        assert_eq!(inverse_relation("teaches"), Some("taught_by"));
        assert_eq!(inverse_relation("taught_by"), Some("teaches"));
    }

    #[test]
    fn test_inverse_unknown() {
        assert_eq!(inverse_relation("unknown_relation"), None);
    }

    #[test]
    fn test_with_inverses_symmetric() {
        let rels = extract_relationships("Alice and Bob are friends");
        let bidirectional = with_inverses(&rels);
        assert_eq!(bidirectional.len(), 2);
        assert_eq!(bidirectional[0].person_a, "Alice");
        assert_eq!(bidirectional[0].person_b, "Bob");
        assert_eq!(bidirectional[1].person_a, "Bob");
        assert_eq!(bidirectional[1].person_b, "Alice");
        assert_eq!(bidirectional[1].relation, "friend_of");
    }

    #[test]
    fn test_with_inverses_asymmetric() {
        let rels = vec![InferredRelationship {
            person_a: "Alice".to_string(),
            person_b: "Bob".to_string(),
            relation: "manages".to_string(),
            confidence: 0.8,
        }];
        let bidirectional = with_inverses(&rels);
        assert_eq!(bidirectional.len(), 2);
        assert_eq!(bidirectional[0].relation, "manages");
        assert_eq!(bidirectional[1].person_a, "Bob");
        assert_eq!(bidirectional[1].person_b, "Alice");
        assert_eq!(bidirectional[1].relation, "reports_to");
    }
}
