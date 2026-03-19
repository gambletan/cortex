//! Proactive inference — automatically extract structured knowledge from raw text.
//!
//! When text is ingested, the inference engine extracts:
//! - Facts (subject-predicate-object triples)
//! - Preferences (key-value pairs with confidence)
//! - Temporal hints (temporary vs permanent)
//!
//! This runs locally using pattern matching and heuristics — no LLM calls.
//! Designed to be fast enough to run on every ingest (<1ms).
//!
//! Optionally, an external LLM callback (`InferenceFn`) can be provided for
//! higher-quality extraction. When set, LLM results are merged with pattern
//! matching results. Default: disabled (pattern matching only).

use crate::CortexError;

/// External LLM inference callback type.
/// Takes raw text, returns extracted knowledge.
/// Opt-in: when not provided, only pattern matching is used.
pub type InferenceFn =
    Box<dyn Fn(&str) -> Result<InferredKnowledge, CortexError> + Send + Sync>;

/// Extract with optional LLM assistance.
/// When `llm_fn` is provided, calls it and merges results with pattern matching.
/// When `None`, falls back to pure pattern matching (`extract()`).
pub fn extract_with_llm(
    text: &str,
    llm_fn: Option<&InferenceFn>,
) -> InferredKnowledge {
    let mut base = extract(text);

    if let Some(f) = llm_fn {
        match f(text) {
            Ok(llm_result) => {
                // Merge LLM results: add non-duplicate facts and preferences
                for fact in llm_result.facts {
                    let dominated = base.facts.iter().any(|f| {
                        f.subject == fact.subject
                            && f.predicate == fact.predicate
                            && f.object == fact.object
                    });
                    if !dominated {
                        base.facts.push(fact);
                    }
                }
                for pref in llm_result.preferences {
                    let dominated = base.preferences.iter().any(|p| {
                        p.key == pref.key && p.value == pref.value
                    });
                    if !dominated {
                        base.preferences.push(pref);
                    }
                }
                // LLM temporal hint overrides pattern matching if it has a signal
                if llm_result.temporal_hint != TemporalHint::Unknown {
                    base.temporal_hint = llm_result.temporal_hint;
                }
            }
            Err(e) => {
                tracing::warn!("LLM inference failed, using pattern matching only: {}", e);
            }
        }
    }

    base
}

/// Extracted knowledge from a text input.
#[derive(Debug, Clone, Default)]
pub struct InferredKnowledge {
    pub facts: Vec<InferredFact>,
    pub preferences: Vec<InferredPreference>,
    pub temporal_hint: TemporalHint,
}

#[derive(Debug, Clone)]
pub struct InferredFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct InferredPreference {
    pub key: String,
    pub value: String,
    pub confidence: f32,
}

/// Hint about whether a memory is temporary or permanent.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TemporalHint {
    /// No strong signal either way.
    #[default]
    Unknown,
    /// Likely temporary (e.g., "I'm working on X right now").
    Temporary,
    /// Likely permanent (e.g., "I live in Shanghai").
    Permanent,
}

/// Extract structured knowledge from raw text.
/// Supports both English and Chinese.
/// Designed to be fast (<1ms) — runs on every ingest.
pub fn extract(text: &str) -> InferredKnowledge {
    let mut knowledge = InferredKnowledge::default();
    let lower = text.to_lowercase();

    // Skip extraction if the sentence is negated
    if !is_negated(&lower, text) {
        // English extraction
        extract_preferences(text, &lower, &mut knowledge);
        extract_facts(text, &lower, &mut knowledge);

        // Chinese extraction
        extract_chinese_facts(text, &mut knowledge);
        extract_chinese_preferences(text, &mut knowledge);
    }

    // Determine temporal hint (both languages) — always run
    knowledge.temporal_hint = classify_temporal_multilingual(text, &lower);

    knowledge
}

/// Detect if the text contains negation that should suppress extraction.
/// "I don't like X" → true, "I don't think it will rain" → true
/// "I live in X" → false
fn is_negated(lower: &str, text: &str) -> bool {
    let en_negations = [
        "i don't ", "i do not ", "i'm not ", "i am not ",
        "i never ", "i can't ", "i cannot ", "i won't ",
        "i didn't ", "i did not ", "i haven't ", "i have not ",
        "i wouldn't ", "i would not ",
    ];
    let zh_negations = [
        "我不", "我没有", "我没", "我不会", "我从不",
        "我不是", "我不在", "我不喜欢", "我不想",
    ];

    for neg in &en_negations {
        if lower.starts_with(neg) {
            return true;
        }
    }
    for neg in &zh_negations {
        if text.starts_with(neg) {
            return true;
        }
    }
    false
}

// ── Preference extraction ────────────────────────────────────────────────────

/// Patterns: "I prefer X", "I like X", "I use X", "my favorite X is Y"
fn extract_preferences(text: &str, lower: &str, knowledge: &mut InferredKnowledge) {
    // "I prefer X over Y" / "I prefer X"
    if let Some(rest) = strip_prefix_any(lower, &["i prefer ", "i always use ", "i usually use "]) {
        let rest_orig = &text[text.len() - rest.len()..]; // preserve original case
        if let Some((a, _b)) = rest.split_once(" over ") {
            knowledge.preferences.push(InferredPreference {
                key: "preference".into(),
                value: clean_value(a),
                confidence: 0.85,
            });
        } else {
            let value = clean_value(first_clause(rest_orig));
            if !value.is_empty() && value.len() < 100 {
                knowledge.preferences.push(InferredPreference {
                    key: "preference".into(),
                    value,
                    confidence: 0.80,
                });
            }
        }
    }

    // "I like X" / "I love X"
    if let Some(rest) = strip_prefix_any(lower, &["i like ", "i love ", "i enjoy "]) {
        let value = clean_value(first_clause(rest));
        if !value.is_empty() && value.len() < 100 {
            knowledge.preferences.push(InferredPreference {
                key: "likes".into(),
                value,
                confidence: 0.75,
            });
        }
    }

    // "my favorite X is Y"
    if let Some(rest) = strip_prefix_any(lower, &["my favorite ", "my preferred "]) {
        if let Some((key_part, value_part)) = rest.split_once(" is ") {
            let key = clean_value(key_part);
            let value = clean_value(first_clause(value_part));
            if !key.is_empty() && !value.is_empty() {
                knowledge.preferences.push(InferredPreference {
                    key,
                    value,
                    confidence: 0.90,
                });
            }
        }
    }

    // Tech stack patterns: "I use X for Y"
    if let Some(rest) = strip_prefix_any(lower, &["i use "]) {
        if let Some((tool, purpose)) = rest.split_once(" for ") {
            let tool = clean_value(tool);
            let purpose = clean_value(first_clause(purpose));
            if !tool.is_empty() && tool.len() < 50 {
                knowledge.preferences.push(InferredPreference {
                    key: format!("tool_for_{}", purpose),
                    value: tool,
                    confidence: 0.80,
                });
            }
        }
    }

    // "I'm into X", "I'm passionate about X", "I mainly use X"
    if let Some(rest) = strip_prefix_any(lower, &["i'm into ", "i am into "]) {
        let value = clean_value(first_clause(rest));
        if !value.is_empty() && value.len() < 100 {
            knowledge.preferences.push(InferredPreference {
                key: "interest".into(),
                value,
                confidence: 0.75,
            });
        }
    }
    if let Some(rest) = strip_prefix_any(lower, &["i'm passionate about ", "i am passionate about "]) {
        let value = clean_value(first_clause(rest));
        if !value.is_empty() && value.len() < 100 {
            knowledge.preferences.push(InferredPreference {
                key: "passion".into(),
                value,
                confidence: 0.80,
            });
        }
    }
    if let Some(rest) = strip_prefix_any(lower, &["i mainly use "]) {
        let value = clean_value(first_clause(rest));
        if !value.is_empty() && value.len() < 100 {
            knowledge.preferences.push(InferredPreference {
                key: "preference".into(),
                value,
                confidence: 0.80,
            });
        }
    }

    // Language patterns
    for (pattern, lang_key) in &[
        ("i speak ", "language"),
        ("i code in ", "programming_language"),
        ("i program in ", "programming_language"),
        ("i write ", "programming_language"),
    ] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let value = clean_value(first_clause(rest));
            if !value.is_empty() && value.len() < 50 {
                knowledge.preferences.push(InferredPreference {
                    key: lang_key.to_string(),
                    value,
                    confidence: 0.85,
                });
            }
        }
    }
}

// ── Fact extraction ──────────────────────────────────────────────────────────

/// Patterns: "I live in X", "I work at X", "I am a X", "my name is X"
fn extract_facts(_text: &str, lower: &str, knowledge: &mut InferredKnowledge) {
    // Location: "I live in X", "I'm based in X", "I'm from X"
    for (pattern, predicate) in &[
        ("i live in ", "lives_in"),
        ("i'm based in ", "based_in"),
        ("i am based in ", "based_in"),
        ("i'm from ", "from"),
        ("i am from ", "from"),
        ("i moved to ", "lives_in"),
    ] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let object = clean_value(first_clause(rest));
            if !object.is_empty() && object.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: predicate.to_string(),
                    object: capitalize_first(&object),
                    confidence: 0.85,
                });
            }
        }
    }

    // Work: "I work at X", "I work for X", "I'm a X at Y"
    for (pattern, predicate) in &[
        ("i work at ", "works_at"),
        ("i work for ", "works_at"),
        ("i work on ", "works_on"),
    ] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let object = clean_value(first_clause(rest));
            if !object.is_empty() && object.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: predicate.to_string(),
                    object: capitalize_first(&object),
                    confidence: 0.85,
                });
            }
        }
    }

    // Identity: "I am a X", "I'm a X"
    for pattern in &["i am a ", "i'm a ", "i am an ", "i'm an "] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let object = clean_value(first_clause(rest));
            if !object.is_empty() && object.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: "is_a".into(),
                    object,
                    confidence: 0.80,
                });
            }
        }
    }

    // Name: "my name is X", "I'm X" (only if short, likely a name)
    if let Some(rest) = strip_prefix_any(lower, &["my name is ", "call me "]) {
        let name = clean_value(first_clause(rest));
        if !name.is_empty() && name.len() < 40 && name.split_whitespace().count() <= 3 {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "name".into(),
                object: capitalize_first(&name),
                confidence: 0.90,
            });
        }
    }

    // "I have ..." — dispatch to experience, children, or pet
    if let Some(rest) = strip_prefix_any(lower, &["i have "]) {
        if rest.contains("years of experience") || rest.contains("years experience") {
            // Age/experience: "I have X years of experience"
            let years_part = first_clause(rest);
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "experience".into(),
                object: clean_value(years_part),
                confidence: 0.80,
            });
        } else if rest.contains("kid") || rest.contains("child") || rest.contains("children") {
            // Children: "I have 2 kids"
            let object = clean_value(first_clause(rest));
            if !object.is_empty() {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: "has_children".into(),
                    object,
                    confidence: 0.85,
                });
            }
        } else {
            // Pet: "I have a dog", "I have two cats", "I have an iguana"
            let article_stripped = rest
                .strip_prefix("a ")
                .or_else(|| rest.strip_prefix("an "))
                .unwrap_or(rest);
            let object = clean_value(first_clause(article_stripped));
            let pet_words = [
                "dog", "cat", "pet", "bird", "fish", "hamster", "rabbit",
                "parrot", "turtle", "snake", "guinea pig", "ferret", "lizard",
                "iguana", "gerbil", "chinchilla", "kitten", "puppy",
            ];
            let object_lower = object.to_lowercase();
            if pet_words.iter().any(|w| object_lower.contains(w)) {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: "has_pet".into(),
                    object,
                    confidence: 0.85,
                });
            }
        }
    }

    // Timezone: "my timezone is X", "I'm in X timezone"
    if let Some(rest) = strip_prefix_any(lower, &["my timezone is ", "my time zone is "]) {
        let tz = clean_value(first_clause(rest));
        if !tz.is_empty() {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "timezone".into(),
                object: tz,
                confidence: 0.90,
            });
        }
    }

    // Education: "I studied X", "I graduated from X", "I have a degree in X", "I majored in X"
    for (pattern, predicate) in &[
        ("i studied at ", "studied_at"),
        ("i studied ", "degree_in"),
        ("i graduated from ", "studied_at"),
        ("i have a degree in ", "degree_in"),
        ("i majored in ", "degree_in"),
    ] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            // Preserve original case from the input text
            let orig_rest = &_text[_text.len() - rest.len()..];
            let object = clean_value(first_clause(orig_rest));
            if !object.is_empty() && object.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: predicate.to_string(),
                    object,
                    confidence: 0.85,
                });
            }
        }
    }

    // Hobbies: "my hobby is X", "I enjoy doing X", "in my free time I X"
    if let Some(rest) = strip_prefix_any(lower, &["my hobby is ", "my hobbies are ", "i enjoy doing ", "in my free time i "]) {
        let object = clean_value(first_clause(rest));
        if !object.is_empty() && object.len() < 80 {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "hobby".into(),
                object,
                confidence: 0.80,
            });
        }
    }

    // Relationship: "I'm married"
    if lower.starts_with("i'm married") || lower.starts_with("i am married") {
        knowledge.facts.push(InferredFact {
            subject: "User".into(),
            predicate: "marital_status".into(),
            object: "married".into(),
            confidence: 0.90,
        });
    }

    // Native language: "my native language is X", "my mother tongue is X"
    if let Some(rest) = strip_prefix_any(lower, &["my native language is ", "my mother tongue is "]) {
        let object = clean_value(first_clause(rest));
        if !object.is_empty() && object.len() < 50 {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "native_language".into(),
                object: capitalize_first(&object),
                confidence: 0.90,
            });
        }
    }
}

// ── Chinese fact extraction ──────────────────────────────────────────────────

/// Extract facts from Chinese text using pattern matching.
/// Chinese patterns: "我住在X", "我在X工作", "我是X", "我叫X", etc.
fn extract_chinese_facts(text: &str, knowledge: &mut InferredKnowledge) {
    // Location: "我住在X", "我在X住", "我来自X", "我搬到了X"
    for (prefix, suffix, predicate) in &[
        ("我住在", "", "lives_in"),
        ("我居住在", "", "lives_in"),
        ("我来自", "", "from"),
        ("我老家是", "", "from"),
        ("我老家在", "", "from"),
        ("我搬到了", "", "lives_in"),
        ("我搬到", "", "lives_in"),
        ("我在", "住", "lives_in"),
        ("我在", "生活", "lives_in"),
    ] {
        if let Some(obj) = extract_zh_pattern(text, prefix, suffix) {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: predicate.to_string(),
                object: obj,
                confidence: 0.85,
            });
        }
    }

    // Work: "我在X工作", "我在X上班"
    for (prefix, suffix, predicate) in &[
        ("我在", "工作", "works_at"),
        ("我在", "上班", "works_at"),
        ("我在", "做", "works_on"),
    ] {
        if let Some(obj) = extract_zh_pattern(text, prefix, suffix) {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: predicate.to_string(),
                object: obj,
                confidence: 0.85,
            });
        }
    }

    // Identity: "我是X", "我是一个X", "我是一名X"
    for prefix in &["我是一名", "我是一个", "我是个", "我是"] {
        if let Some(obj) = extract_zh_prefix(text, prefix) {
            // Skip if it's a location/name pattern (handled separately)
            if !obj.is_empty() && obj.len() < 80 && !obj.starts_with('在') {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: "is_a".into(),
                    object: obj,
                    confidence: 0.80,
                });
                break; // Only match most specific prefix
            }
        }
    }

    // Name: "我叫X", "我的名字是X"
    for prefix in &["我叫", "我的名字是", "叫我", "我名字叫"] {
        if let Some(obj) = extract_zh_prefix(text, prefix) {
            if !obj.is_empty() && obj.chars().count() <= 10 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: "name".into(),
                    object: obj,
                    confidence: 0.90,
                });
                break;
            }
        }
    }

    // Age: "我今年X岁", "我X岁"
    if let Some(obj) = extract_zh_pattern(text, "我今年", "岁") {
        knowledge.facts.push(InferredFact {
            subject: "User".into(),
            predicate: "age".into(),
            object: format!("{}岁", obj),
            confidence: 0.85,
        });
    }

    // Experience: "我有X年经验", "我做了X年"
    if let Some(obj) = extract_zh_pattern(text, "我有", "年经验") {
        knowledge.facts.push(InferredFact {
            subject: "User".into(),
            predicate: "experience".into(),
            object: format!("{}年", obj),
            confidence: 0.80,
        });
    }

    // Education: "我毕业于X", "我在X上学", "我学的是X"
    for (prefix, suffix, predicate) in &[
        ("我毕业于", "", "studied_at"),
        ("我在", "上学", "studied_at"),
        ("我学的是", "", "degree_in"),
    ] {
        if let Some(obj) = extract_zh_pattern(text, prefix, suffix) {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: predicate.to_string(),
                object: obj,
                confidence: 0.85,
            });
        }
    }

    // Hobbies: "我的爱好是X", "我平时喜欢X"
    for prefix in &["我的爱好是", "我平时喜欢"] {
        if let Some(obj) = extract_zh_prefix(text, prefix) {
            if !obj.is_empty() && obj.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: "hobby".into(),
                    object: obj,
                    confidence: 0.80,
                });
                break;
            }
        }
    }

    // Pet: "我养了X"
    if let Some(obj) = extract_zh_prefix(text, "我养了") {
        if !obj.is_empty() && obj.len() < 50 {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "has_pet".into(),
                object: obj,
                confidence: 0.85,
            });
        }
    }

    // Native language: "我的母语是X"
    if let Some(obj) = extract_zh_prefix(text, "我的母语是") {
        if !obj.is_empty() && obj.len() < 50 {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "native_language".into(),
                object: obj,
                confidence: 0.90,
            });
        }
    }
}

// ── Chinese preference extraction ────────────────────────────────────────────

fn extract_chinese_preferences(text: &str, knowledge: &mut InferredKnowledge) {
    // "我喜欢X", "我爱X", "我偏好X"
    for (prefix, key, conf) in &[
        ("我喜欢用", "preference", 0.85_f32),
        ("我喜欢", "likes", 0.75),
        ("我爱用", "preference", 0.85),
        ("我爱", "likes", 0.75),
        ("我偏好", "preference", 0.85),
        ("我习惯用", "preference", 0.80),
        ("我常用", "preference", 0.80),
    ] {
        if let Some(obj) = extract_zh_prefix(text, prefix) {
            if !obj.is_empty() && obj.len() < 100 {
                knowledge.preferences.push(InferredPreference {
                    key: key.to_string(),
                    value: obj,
                    confidence: *conf,
                });
                break; // Only match most specific prefix
            }
        }
    }

    // "我最喜欢的X是Y"
    if let Some(rest) = text.strip_prefix("我最喜欢的") {
        if let Some((key_part, value_part)) = rest.split_once('是') {
            let key = clean_zh_value(key_part);
            let value = clean_zh_value(value_part);
            if !key.is_empty() && !value.is_empty() {
                knowledge.preferences.push(InferredPreference {
                    key,
                    value,
                    confidence: 0.90,
                });
            }
        }
    }

    // "我用X写代码" / "我用X开发"
    if let Some(rest) = text.strip_prefix("我用") {
        for suffix in &["写代码", "开发", "编程", "写程序"] {
            if let Some(pos) = rest.find(suffix) {
                let tool = clean_zh_value(&rest[..pos]);
                if !tool.is_empty() && tool.len() < 50 {
                    knowledge.preferences.push(InferredPreference {
                        key: "programming_tool".into(),
                        value: tool,
                        confidence: 0.80,
                    });
                    break;
                }
            }
        }
    }

    // "我觉得X很好", "X很好用"
    if let Some(rest) = text.strip_prefix("我觉得") {
        for suffix in &["很好", "不错", "好用", "方便"] {
            if let Some(pos) = rest.find(suffix) {
                let value = clean_zh_value(&rest[..pos]);
                if !value.is_empty() && value.len() < 50 {
                    knowledge.preferences.push(InferredPreference {
                        key: "likes".into(),
                        value,
                        confidence: 0.70,
                    });
                    break;
                }
            }
        }
    }

    // "我擅长X", "我熟悉X"
    for (prefix, key) in &[("我擅长", "skill"), ("我熟悉", "familiar_with"), ("我精通", "skill")] {
        if let Some(obj) = extract_zh_prefix(text, prefix) {
            if !obj.is_empty() && obj.len() < 100 {
                knowledge.preferences.push(InferredPreference {
                    key: key.to_string(),
                    value: obj,
                    confidence: 0.80,
                });
                break;
            }
        }
    }

    // "我主要用X", "我经常用X"
    for prefix in &["我主要用", "我经常用"] {
        if let Some(obj) = extract_zh_prefix(text, prefix) {
            if !obj.is_empty() && obj.len() < 100 {
                knowledge.preferences.push(InferredPreference {
                    key: "preference".into(),
                    value: obj,
                    confidence: 0.80,
                });
                break;
            }
        }
    }

    // Language: "我会说X", "我说X"
    for prefix in &["我会说", "我说", "我会讲"] {
        if let Some(obj) = extract_zh_prefix(text, prefix) {
            if !obj.is_empty() && obj.len() < 50 {
                knowledge.preferences.push(InferredPreference {
                    key: "language".into(),
                    value: obj,
                    confidence: 0.85,
                });
                break;
            }
        }
    }
}

// ── Temporal classification (multilingual) ───────────────────────────────────

fn classify_temporal_multilingual(text: &str, lower: &str) -> TemporalHint {
    // English temporary signals
    let en_temporary = [
        "right now", "currently", "at the moment", "today",
        "this week", "this morning", "this afternoon", "this evening",
        "working on", "debugging", "trying to", "about to",
        "just finished", "just started", "in the middle of",
        "temporarily", "for now",
    ];

    // English permanent signals
    let en_permanent = [
        "always", "i am a", "i'm a", "i live", "i work at",
        "my name is", "i speak", "i prefer", "my favorite",
        "i was born", "i have been", "for years", "since",
        "i never", "i hate", "i love",
    ];

    // Chinese temporary signals
    let zh_temporary = [
        "现在", "目前", "正在", "今天", "这周", "这个星期",
        "今早", "今晚", "刚刚", "刚才", "马上",
        "暂时", "临时", "先", "在调试", "在修",
    ];

    // Chinese permanent signals
    let zh_permanent = [
        "我住在", "我在", "工作", "我叫", "我是",
        "我喜欢", "我偏好", "一直", "总是", "从来",
        "我来自", "永远", "多年", "我会说",
    ];

    let temp_score: f32 = en_temporary.iter()
        .filter(|s| lower.contains(**s))
        .count() as f32
        + zh_temporary.iter()
        .filter(|s| text.contains(**s))
        .count() as f32;

    let perm_score: f32 = en_permanent.iter()
        .filter(|s| lower.contains(**s))
        .count() as f32
        + zh_permanent.iter()
        .filter(|s| text.contains(**s))
        .count() as f32;

    if temp_score > perm_score && temp_score >= 1.0 {
        TemporalHint::Temporary
    } else if perm_score > temp_score && perm_score >= 1.0 {
        TemporalHint::Permanent
    } else {
        TemporalHint::Unknown
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Try multiple prefixes, return the rest after the first match.
fn strip_prefix_any<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for prefix in prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    None
}

/// Get text up to the first sentence/clause boundary.
fn first_clause(text: &str) -> &str {
    // Check punctuation boundaries
    let punct_end = text.find(&['.', ',', '!', '?', '\n', ';'][..])
        .unwrap_or(text.len());
    // Also check " and " as a clause boundary
    let and_end = text.find(" and ")
        .unwrap_or(text.len());
    let end = punct_end.min(and_end);
    &text[..end]
}

/// Clean whitespace and trailing punctuation from a value.
fn clean_value(s: &str) -> String {
    s.trim()
        .trim_end_matches(&['.', ',', '!', '?', ';'][..])
        .trim()
        .to_string()
}

/// Extract value between a Chinese prefix and suffix.
/// e.g., extract_zh_pattern("我在上海住", "我在", "住") -> Some("上海")
fn extract_zh_pattern(text: &str, prefix: &str, suffix: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix(prefix) {
        if suffix.is_empty() {
            let val = clean_zh_value(first_zh_clause(rest));
            if !val.is_empty() {
                return Some(val);
            }
        } else if let Some(pos) = rest.find(suffix) {
            let val = clean_zh_value(&rest[..pos]);
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    // Also search mid-sentence
    if !prefix.is_empty() {
        if let Some(start) = text.find(prefix) {
            let after = &text[start + prefix.len()..];
            if suffix.is_empty() {
                let val = clean_zh_value(first_zh_clause(after));
                if !val.is_empty() {
                    return Some(val);
                }
            } else if let Some(pos) = after.find(suffix) {
                let val = clean_zh_value(&after[..pos]);
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Extract value after a Chinese prefix up to first clause boundary.
fn extract_zh_prefix(text: &str, prefix: &str) -> Option<String> {
    // Try from start
    if let Some(rest) = text.strip_prefix(prefix) {
        let val = clean_zh_value(first_zh_clause(rest));
        if !val.is_empty() {
            return Some(val);
        }
    }
    // Try mid-sentence
    if let Some(start) = text.find(prefix) {
        let after = &text[start + prefix.len()..];
        let val = clean_zh_value(first_zh_clause(after));
        if !val.is_empty() {
            return Some(val);
        }
    }
    None
}

/// Get text up to the first Chinese clause boundary.
fn first_zh_clause(text: &str) -> &str {
    let boundaries = ['，', '。', '！', '？', '；', '、', '\n', ',', '.', '!', '?'];
    let end = text.find(&boundaries[..]).unwrap_or(text.len());
    &text[..end]
}

/// Clean whitespace and trailing Chinese punctuation.
fn clean_zh_value(s: &str) -> String {
    s.trim()
        .trim_end_matches(&['，', '。', '！', '？', '；', '、', '.', ',', '!', '?', ';', '了', '的', '呢', '啊', '吧'][..])
        .trim()
        .to_string()
}

fn capitalize_first(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap().to_uppercase().to_string();
    first + chars.as_str()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_preference_basic() {
        let k = extract("I prefer Rust over Python");
        assert_eq!(k.preferences.len(), 1);
        assert_eq!(k.preferences[0].value, "rust");
        assert!(k.preferences[0].confidence > 0.8);
    }

    #[test]
    fn test_extract_favorite() {
        let k = extract("My favorite editor is neovim");
        assert_eq!(k.preferences.len(), 1);
        assert_eq!(k.preferences[0].key, "editor");
        assert_eq!(k.preferences[0].value, "neovim");
    }

    #[test]
    fn test_extract_fact_location() {
        let k = extract("I live in Shanghai");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "lives_in");
        assert_eq!(k.facts[0].object, "Shanghai");
    }

    #[test]
    fn test_extract_fact_work() {
        let k = extract("I work at Google");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "works_at");
        assert_eq!(k.facts[0].object, "Google");
    }

    #[test]
    fn test_extract_fact_identity() {
        let k = extract("I'm a software engineer");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "is_a");
        assert_eq!(k.facts[0].object, "software engineer");
    }

    #[test]
    fn test_extract_name() {
        let k = extract("My name is Alvin");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "name");
        assert_eq!(k.facts[0].object, "Alvin");
    }

    #[test]
    fn test_temporal_temporary() {
        let k = extract("I'm currently working on a Rust project");
        assert_eq!(k.temporal_hint, TemporalHint::Temporary);
    }

    #[test]
    fn test_temporal_permanent() {
        let k = extract("I live in Shanghai and I'm a developer");
        assert_eq!(k.temporal_hint, TemporalHint::Permanent);
    }

    #[test]
    fn test_no_extraction_from_noise() {
        let k = extract("The weather is nice today");
        assert!(k.facts.is_empty());
        assert!(k.preferences.is_empty());
    }

    #[test]
    fn test_programming_language() {
        let k = extract("I code in Rust and Python");
        assert!(!k.preferences.is_empty());
        assert_eq!(k.preferences[0].key, "programming_language");
    }

    #[test]
    fn test_use_tool_for() {
        let k = extract("I use neovim for editing");
        assert!(!k.preferences.is_empty());
        assert!(k.preferences[0].key.contains("tool_for"));
    }

    #[test]
    fn test_negation_english() {
        let k = extract("I don't like spicy food");
        assert!(k.preferences.is_empty(), "Negated preference should not be extracted");
    }

    #[test]
    fn test_negation_chinese() {
        let k = extract("我不喜欢辣的");
        assert!(k.preferences.is_empty(), "Chinese negation should suppress extraction");
    }

    #[test]
    fn test_negation_doesnt_block_temporal() {
        let k = extract("I don't live here right now");
        // Facts should be empty (negated), but temporal hint should still work
        assert!(k.facts.is_empty());
        assert_eq!(k.temporal_hint, TemporalHint::Temporary);
    }

    // ── Chinese tests ────────────────────────────────────────────────────

    #[test]
    fn test_zh_location() {
        let k = extract("我住在上海");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "lives_in");
        assert_eq!(k.facts[0].object, "上海");
    }

    #[test]
    fn test_zh_location_from() {
        let k = extract("我来自北京");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "from");
        assert_eq!(k.facts[0].object, "北京");
    }

    #[test]
    fn test_zh_work() {
        let k = extract("我在谷歌工作");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "works_at");
        assert_eq!(k.facts[0].object, "谷歌");
    }

    #[test]
    fn test_zh_identity() {
        let k = extract("我是一名软件工程师");
        assert!(!k.facts.is_empty());
        let fact = k.facts.iter().find(|f| f.predicate == "is_a").unwrap();
        assert_eq!(fact.object, "软件工程师");
    }

    #[test]
    fn test_zh_name() {
        let k = extract("我叫小明");
        assert!(!k.facts.is_empty());
        let fact = k.facts.iter().find(|f| f.predicate == "name").unwrap();
        assert_eq!(fact.object, "小明");
    }

    #[test]
    fn test_zh_preference() {
        let k = extract("我喜欢用Rust");
        assert!(!k.preferences.is_empty());
        assert_eq!(k.preferences[0].value, "Rust");
    }

    #[test]
    fn test_zh_favorite() {
        let k = extract("我最喜欢的编辑器是neovim");
        assert!(!k.preferences.is_empty());
        assert_eq!(k.preferences[0].key, "编辑器");
        assert_eq!(k.preferences[0].value, "neovim");
    }

    #[test]
    fn test_zh_temporal_temporary() {
        let k = extract("我正在调试一个bug");
        assert_eq!(k.temporal_hint, TemporalHint::Temporary);
    }

    #[test]
    fn test_zh_temporal_permanent() {
        let k = extract("我住在上海，我是程序员");
        assert_eq!(k.temporal_hint, TemporalHint::Permanent);
    }

    #[test]
    fn test_zh_no_extraction_from_noise() {
        let k = extract("今天天气不错");
        assert!(k.facts.is_empty());
        assert!(k.preferences.is_empty());
    }

    #[test]
    fn test_mixed_zh_en() {
        let k = extract("I live in Shanghai，我喜欢用Rust");
        assert!(!k.facts.is_empty()); // English: lives_in Shanghai
        assert!(!k.preferences.is_empty()); // Chinese: likes Rust
    }

    // ── New English fact pattern tests ─────────────────────────────────

    #[test]
    fn test_extract_education_studied() {
        let k = extract("I studied Computer Science");
        let fact = k.facts.iter().find(|f| f.predicate == "degree_in").unwrap();
        assert_eq!(fact.object, "Computer Science");
    }

    #[test]
    fn test_extract_education_graduated() {
        let k = extract("I graduated from MIT");
        let fact = k.facts.iter().find(|f| f.predicate == "studied_at").unwrap();
        assert_eq!(fact.object, "MIT");
    }

    #[test]
    fn test_extract_education_degree() {
        let k = extract("I have a degree in Physics");
        let fact = k.facts.iter().find(|f| f.predicate == "degree_in").unwrap();
        assert_eq!(fact.object, "Physics");
    }

    #[test]
    fn test_extract_education_majored() {
        let k = extract("I majored in Mathematics");
        let fact = k.facts.iter().find(|f| f.predicate == "degree_in").unwrap();
        assert_eq!(fact.object, "Mathematics");
    }

    #[test]
    fn test_extract_hobby() {
        let k = extract("My hobby is painting");
        let fact = k.facts.iter().find(|f| f.predicate == "hobby").unwrap();
        assert_eq!(fact.object, "painting");
    }

    #[test]
    fn test_extract_hobby_enjoy_doing() {
        let k = extract("I enjoy doing yoga");
        let fact = k.facts.iter().find(|f| f.predicate == "hobby").unwrap();
        assert_eq!(fact.object, "yoga");
    }

    #[test]
    fn test_extract_hobby_free_time() {
        let k = extract("In my free time I read books");
        let fact = k.facts.iter().find(|f| f.predicate == "hobby").unwrap();
        assert_eq!(fact.object, "read books");
    }

    #[test]
    fn test_extract_married() {
        let k = extract("I'm married");
        let fact = k.facts.iter().find(|f| f.predicate == "marital_status").unwrap();
        assert_eq!(fact.object, "married");
    }

    #[test]
    fn test_extract_children() {
        let k = extract("I have 2 kids");
        let fact = k.facts.iter().find(|f| f.predicate == "has_children").unwrap();
        assert_eq!(fact.object, "2 kids");
    }

    #[test]
    fn test_extract_pet() {
        let k = extract("I have a dog");
        let fact = k.facts.iter().find(|f| f.predicate == "has_pet").unwrap();
        assert_eq!(fact.object, "dog");
    }

    #[test]
    fn test_extract_pet_cat() {
        let k = extract("I have a cat");
        let fact = k.facts.iter().find(|f| f.predicate == "has_pet").unwrap();
        assert_eq!(fact.object, "cat");
    }

    #[test]
    fn test_extract_native_language() {
        let k = extract("My native language is Chinese");
        let fact = k.facts.iter().find(|f| f.predicate == "native_language").unwrap();
        assert_eq!(fact.object, "Chinese");
    }

    #[test]
    fn test_extract_mother_tongue() {
        let k = extract("My mother tongue is Japanese");
        let fact = k.facts.iter().find(|f| f.predicate == "native_language").unwrap();
        assert_eq!(fact.object, "Japanese");
    }

    // ── New English preference pattern tests ──────────────────────────

    #[test]
    fn test_extract_pref_into() {
        let k = extract("I'm into machine learning");
        let pref = k.preferences.iter().find(|p| p.key == "interest").unwrap();
        assert_eq!(pref.value, "machine learning");
        assert!((pref.confidence - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_extract_pref_passionate() {
        let k = extract("I'm passionate about open source");
        let pref = k.preferences.iter().find(|p| p.key == "passion").unwrap();
        assert_eq!(pref.value, "open source");
        assert!((pref.confidence - 0.80).abs() < 0.01);
    }

    #[test]
    fn test_extract_pref_mainly_use() {
        let k = extract("I mainly use Linux");
        let pref = k.preferences.iter().find(|p| p.key == "preference" && p.value == "linux").unwrap();
        assert!((pref.confidence - 0.80).abs() < 0.01);
    }

    // ── New Chinese fact pattern tests ────────────────────────────────

    #[test]
    fn test_zh_education_graduated() {
        let k = extract("我毕业于清华大学");
        let fact = k.facts.iter().find(|f| f.predicate == "studied_at").unwrap();
        assert_eq!(fact.object, "清华大学");
    }

    #[test]
    fn test_zh_education_school() {
        let k = extract("我在北大上学");
        let fact = k.facts.iter().find(|f| f.predicate == "studied_at").unwrap();
        assert_eq!(fact.object, "北大");
    }

    #[test]
    fn test_zh_education_degree() {
        let k = extract("我学的是计算机");
        let fact = k.facts.iter().find(|f| f.predicate == "degree_in").unwrap();
        assert_eq!(fact.object, "计算机");
    }

    #[test]
    fn test_zh_hobby() {
        let k = extract("我的爱好是跑步");
        let fact = k.facts.iter().find(|f| f.predicate == "hobby").unwrap();
        assert_eq!(fact.object, "跑步");
    }

    #[test]
    fn test_zh_hobby_usually() {
        let k = extract("我平时喜欢看书");
        let fact = k.facts.iter().find(|f| f.predicate == "hobby").unwrap();
        assert_eq!(fact.object, "看书");
    }

    #[test]
    fn test_zh_pet() {
        let k = extract("我养了一只猫");
        let fact = k.facts.iter().find(|f| f.predicate == "has_pet").unwrap();
        assert_eq!(fact.object, "一只猫");
    }

    #[test]
    fn test_zh_native_language() {
        let k = extract("我的母语是中文");
        let fact = k.facts.iter().find(|f| f.predicate == "native_language").unwrap();
        assert_eq!(fact.object, "中文");
    }

    // ── New Chinese preference pattern tests ──────────────────────────

    #[test]
    fn test_zh_pref_mainly_use() {
        let k = extract("我主要用VS Code");
        let pref = k.preferences.iter().find(|p| p.key == "preference").unwrap();
        assert_eq!(pref.value, "VS Code");
        assert!((pref.confidence - 0.80).abs() < 0.01);
    }

    #[test]
    fn test_zh_pref_often_use() {
        let k = extract("我经常用Python");
        let pref = k.preferences.iter().find(|p| p.key == "preference").unwrap();
        assert_eq!(pref.value, "Python");
        assert!((pref.confidence - 0.80).abs() < 0.01);
    }
}
