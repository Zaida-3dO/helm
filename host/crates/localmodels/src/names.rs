//! The short name a local model goes by in the picker and on a chat's chip, when the user has not
//! given it one: the family, the version and the size, without the quantisation, "instruct" or
//! tags ("qwen3.8-flash-next-iq3_s" reads "Qwen 3.8"). Pure, so a table test covers it.

/// Words that say nothing about which model it is.
const NOISE: &[&str] = &["instruct", "chat", "latest", "gguf", "it", "text"];

/// A quantisation or file-format token: `q4_k_m`, `iq3_s`, `fp16`, `bf16`, `int8`, `awq`...
fn is_quant(t: &str) -> bool {
    let head = t.split('_').next().unwrap_or(t);
    let digits_after = |prefix: &str| {
        head.strip_prefix(prefix)
            .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
    };
    ["q", "iq", "fp", "bf", "f", "int"]
        .iter()
        .any(|p| digits_after(p))
        || matches!(head, "awq" | "gptq" | "gguf" | "mlx")
}

/// `7b`, `32b`, `0.6b`, `270m`: a parameter count.
fn size_of(t: &str) -> Option<String> {
    let (num, unit) = t.split_at(t.len().checked_sub(1)?);
    let ok = matches!(unit, "b" | "m")
        && !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && num.bytes().next().is_some_and(|b| b.is_ascii_digit());
    ok.then(|| format!("{num}{}", unit.to_ascii_uppercase()))
}

fn is_version(t: &str) -> bool {
    !t.is_empty()
        && t.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && t.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && !t.ends_with('.')
}

fn capitalised(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// A model's name split into what it is called (`base`) and the words that tell it from its
/// siblings (`extras`, in order).
struct Parts {
    base: String,
    extras: Vec<String>,
}

fn parts(model: &str) -> Parts {
    // The last path segment: `hf.co/org/name:tag` is `name:tag`.
    let name = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".gguf").unwrap_or(&name);
    let mut tokens: Vec<String> = name
        .split(['-', ':', ' ', '@'])
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    // `qwen3.8` is the family `qwen` and the version `3.8`.
    let (mut family, mut version) = (String::new(), None);
    if let Some(first) = tokens.first().cloned() {
        let at = first
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(first.len());
        let (letters, rest) = first.split_at(at);
        if !letters.is_empty() {
            family = letters.trim_end_matches(['.', '_']).to_owned();
            if is_version(rest) {
                version = Some(rest.to_owned());
            } else if !rest.is_empty() {
                // `qwen3_5` or `phi4mini`: keep the whole token as written.
                family = first;
            }
            tokens.remove(0);
        }
    }
    // `gpt-oss-20b` has no version in its first word; `llama-3.2-3b` has one in its second.
    if version.is_none() && tokens.first().is_some_and(|t| is_version(t)) {
        version = Some(tokens.remove(0));
    }
    let mut size = None;
    let mut extras = Vec::new();
    let mut in_quant = false;
    for t in tokens {
        if is_quant(&t) {
            in_quant = true;
            continue;
        }
        // The `k` and `m` of `q4-k-m` when a quantisation is written with dashes, and filler.
        if (in_quant && t.len() <= 2) || NOISE.contains(&t.as_str()) {
            continue;
        }
        if size.is_none() && size_of(&t).is_some() {
            size = size_of(&t);
            continue;
        }
        in_quant = false;
        extras.push(capitalised(&t));
    }
    let mut base: Vec<String> = Vec::new();
    if !family.is_empty() {
        base.push(capitalised(&family));
    }
    base.extend(version);
    base.extend(size);
    if base.is_empty() {
        // Nothing to go on: the name as the server gives it.
        return Parts {
            base: model.to_owned(),
            extras: Vec::new(),
        };
    }
    Parts {
        base: base.join(" "),
        extras,
    }
}

/// The default short name of one model.
pub fn default_alias(model: &str) -> String {
    parts(model).base
}

/// The default short names of several models listed together. Names that come out the same each
/// take their model's next distinguishing word, until they differ or the words run out.
pub fn default_aliases(models: &[&str]) -> Vec<String> {
    let all: Vec<Parts> = models.iter().map(|m| parts(m)).collect();
    let mut used = vec![0usize; all.len()];
    let name = |i: usize, used: &[usize]| {
        std::iter::once(all[i].base.as_str())
            .chain(all[i].extras.iter().take(used[i]).map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    };
    loop {
        let names: Vec<String> = (0..all.len()).map(|i| name(i, &used)).collect();
        let mut grew = false;
        for i in 0..all.len() {
            let clashes = names
                .iter()
                .enumerate()
                .any(|(j, n)| j != i && *n == names[i] && models[j] != models[i]);
            if clashes && used[i] < all[i].extras.len() {
                used[i] += 1;
                grew = true;
            }
        }
        if !grew {
            return names;
        }
    }
}
