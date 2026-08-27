//! Docker-style display names (`adjective_surname`), persisted next to identity.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Filename stored beside `identity.key` (worker wasm cache / gateway identity dir).
pub const DISPLAY_NAME_FILE: &str = "display_name";

/// Written after a successful registry join/enroll so later starts can skip the token.
pub const REGISTRY_JOINED_FILE: &str = "registry-joined";

/// `<dir>/registry-joined`.
pub fn registry_joined_path(dir: &Path) -> PathBuf {
    dir.join(REGISTRY_JOINED_FILE)
}

/// True after a successful enroll/join against the registry.
pub fn is_registry_joined(dir: &Path) -> bool {
    registry_joined_path(dir).is_file()
}

/// Persist the enrollment marker next to `identity.key`.
pub fn mark_registry_joined(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create `{}`", dir.display()))?;
    let path = registry_joined_path(dir);
    fs::write(&path, "1\n").with_context(|| format!("write `{}`", path.display()))?;
    Ok(())
}

/// Drop the enrollment marker (local identity remove).
pub fn clear_registry_joined(dir: &Path) -> Result<()> {
    let path = registry_joined_path(dir);
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("remove `{}`", path.display()))?;
    }
    Ok(())
}

/// Left half — inspired by Docker's `names-generator` adjectives.
const ADJECTIVES: &[&str] = &[
    "admiring",
    "adoring",
    "affectionate",
    "agitated",
    "amazing",
    "angry",
    "awesome",
    "blissful",
    "bold",
    "boring",
    "brave",
    "busy",
    "charming",
    "clever",
    "compassionate",
    "competent",
    "condescending",
    "confident",
    "cool",
    "cranky",
    "crazy",
    "dazzling",
    "determined",
    "distracted",
    "dreamy",
    "eager",
    "ecstatic",
    "elastic",
    "elated",
    "elegant",
    "eloquent",
    "epic",
    "exciting",
    "fervent",
    "festive",
    "flamboyant",
    "focused",
    "friendly",
    "frosty",
    "funny",
    "gallant",
    "gifted",
    "goofy",
    "gracious",
    "great",
    "happy",
    "hardcore",
    "heuristic",
    "hopeful",
    "hungry",
    "infallible",
    "inspiring",
    "intelligent",
    "interesting",
    "jolly",
    "jovial",
    "keen",
    "kind",
    "laughing",
    "loving",
    "lucid",
    "magical",
    "modest",
    "musing",
    "mystifying",
    "naughty",
    "nervous",
    "nice",
    "nifty",
    "nostalgic",
    "objective",
    "optimistic",
    "peaceful",
    "pedantic",
    "pensive",
    "practical",
    "priceless",
    "quirky",
    "quizzical",
    "recursing",
    "relaxed",
    "reverent",
    "romantic",
    "sad",
    "serene",
    "sharp",
    "silly",
    "sleepy",
    "stoic",
    "strange",
    "stupefied",
    "suspicious",
    "sweet",
    "tender",
    "thirsty",
    "trusting",
    "unruffled",
    "upbeat",
    "vibrant",
    "vigilant",
    "vigorous",
    "wizardly",
    "wonderful",
    "xenodochial",
    "youthful",
    "zealous",
    "zen",
];

/// Right half — scientists / hackers (Docker-style).
const SURNAMES: &[&str] = &[
    "agnesi",
    "albattani",
    "allen",
    "almeida",
    "antonelli",
    "archimedes",
    "ardinghelli",
    "aryabhata",
    "austin",
    "babbage",
    "banach",
    "banzai",
    "bardeen",
    "bartik",
    "bassi",
    "beaver",
    "bell",
    "benz",
    "bhabha",
    "bhaskara",
    "blackburn",
    "blackwell",
    "bohr",
    "booth",
    "borg",
    "bose",
    "bouman",
    "boyd",
    "brahmagupta",
    "brattain",
    "brown",
    "buck",
    "burnell",
    "cannon",
    "carson",
    "cartwright",
    "cerf",
    "chandrasekhar",
    "chaplygin",
    "chatelet",
    "chaum",
    "chebyshev",
    "clarke",
    "cohen",
    "colden",
    "cori",
    "cray",
    "curie",
    "curran",
    "darwin",
    "davinci",
    "dewey",
    "dhawan",
    "diffie",
    "dijkstra",
    "dirac",
    "driscoll",
    "dubinsky",
    "easley",
    "edison",
    "einstein",
    "elbakyan",
    "elgamal",
    "elion",
    "ellis",
    "engelbart",
    "euclid",
    "euler",
    "faraday",
    "feistel",
    "fermat",
    "fermi",
    "feynman",
    "franklin",
    "gagarin",
    "galileo",
    "galois",
    "ganguly",
    "gates",
    "gauss",
    "germain",
    "goldberg",
    "goldstine",
    "goldwasser",
    "golick",
    "goodall",
    "gould",
    "greider",
    "grothendieck",
    "haibt",
    "hamilton",
    "haslett",
    "hawking",
    "hellman",
    "heisenberg",
    "hermann",
    "herschel",
    "hertz",
    "hamilton",
    "hodgkin",
    "hoover",
    "hopper",
    "hugle",
    "hypatia",
    "ishizaka",
    "jackson",
    "jang",
    "jennings",
    "jepsen",
    "johnson",
    "joliot",
    "jones",
    "kalam",
    "kapitsa",
    "kare",
    "keldysh",
    "keller",
    "kepler",
    "khayyam",
    "khorana",
    "kilby",
    "kirch",
    "knuth",
    "kowalevski",
    "lalande",
    "lamarr",
    "lamport",
    "leakey",
    "leavitt",
    "lederberg",
    "lehmann",
    "lewin",
    "liskov",
    "lovelace",
    "lumiere",
    "mahavira",
    "mayer",
    "mccarthy",
    "mcclintock",
    "mclaren",
    "mclean",
    "mcnulty",
    "meitner",
    "meninsky",
    "merkle",
    "mestorf",
    "minsky",
    "mirzakhani",
    "morse",
    "murdock",
    "napier",
    "nash",
    "neumann",
    "newton",
    "nightingale",
    "nobel",
    "noether",
    "northcutt",
    "noyce",
    "panini",
    "pare",
    "pascal",
    "pasteur",
    "payne",
    "perlman",
    "pike",
    "poincare",
    "poitras",
    "ptolemy",
    "raman",
    "ramanujan",
    "ride",
    "ritchie",
    "rhodes",
    "roentgen",
    "rosalind",
    "saha",
    "sammet",
    "sanderson",
    "satoshi",
    "shannon",
    "shaw",
    "shirley",
    "shockley",
    "sinoussi",
    "snyder",
    "solomon",
    "spence",
    "stonebraker",
    "swanson",
    "swartz",
    "swirles",
    "taussig",
    "tesla",
    "thompson",
    "torvalds",
    "tu",
    "turing",
    "varahamihira",
    "vaughan",
    "visvesvaraya",
    "volhard",
    "wescoff",
    "wilbur",
    "wiles",
    "williams",
    "williamson",
    "wilson",
    "wing",
    "wozniak",
    "wright",
    "wu",
    "yalow",
    "yonath",
    "zhukovsky",
];

fn entropy_u64() -> u64 {
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_err() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        return nanos ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9);
    }
    u64::from_le_bytes(buf)
}

/// Generate `adjective_surname` (duplicates across nodes are expected / OK).
pub fn generate_docker_style_name() -> String {
    let e = entropy_u64();
    let adj = ADJECTIVES[(e as usize) % ADJECTIVES.len()];
    let surname = SURNAMES[((e >> 32) as usize) % SURNAMES.len()];
    format!("{adj}_{surname}")
}

fn normalize_explicit(name: &str) -> Option<String> {
    let t = name.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.chars().take(64).collect())
}

fn read_persisted(path: &Path) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read display name `{}`", path.display()))?;
    Ok(normalize_explicit(&raw))
}

fn write_persisted(path: &Path, name: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create `{}`", parent.display()))?;
    }
    fs::write(path, format!("{name}\n"))
        .with_context(|| format!("write display name `{}`", path.display()))?;
    Ok(())
}

/// Resolve a stable display name for a node identity directory.
///
/// 1. Non-empty `explicit` (CLI / config) wins for this process and is not written.
/// 2. Else load `<dir>/display_name` if present.
/// 3. Else generate a Docker-style name, persist it, and return it.
pub fn resolve_persistent_display_name(dir: &Path, explicit: Option<&str>) -> Result<String> {
    if let Some(name) = explicit.and_then(normalize_explicit) {
        return Ok(name);
    }
    let path = dir.join(DISPLAY_NAME_FILE);
    if let Some(name) = read_persisted(&path)? {
        return Ok(name);
    }
    let name = generate_docker_style_name();
    write_persisted(&path, &name)?;
    Ok(name)
}

/// Combinatorial size of the word lists (not a uniqueness guarantee).
pub fn name_space_size() -> u64 {
    (ADJECTIVES.len() as u64).saturating_mul(SURNAMES.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn generated_name_matches_pattern() {
        let name = generate_docker_style_name();
        let (left, right) = name.split_once('_').expect("adj_surname");
        assert!(ADJECTIVES.contains(&left), "{left}");
        assert!(SURNAMES.contains(&right), "{right}");
    }

    #[test]
    fn persists_across_calls_without_explicit() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("beenet-display-name-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let a = resolve_persistent_display_name(&dir, None).unwrap();
        let b = resolve_persistent_display_name(&dir, None).unwrap();
        assert_eq!(a, b);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_overrides_persisted() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("beenet-display-name-ex-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let auto = resolve_persistent_display_name(&dir, None).unwrap();
        let named = resolve_persistent_display_name(&dir, Some("my-gw")).unwrap();
        assert_eq!(named, "my-gw");
        assert_ne!(auto, "my-gw");
        // File still holds the auto name.
        let again = resolve_persistent_display_name(&dir, None).unwrap();
        assert_eq!(again, auto);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn name_space_is_nontrivial() {
        assert!(name_space_size() > 10_000);
    }

    #[test]
    fn registry_joined_marker_roundtrip() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("beenet-registry-joined-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        assert!(!is_registry_joined(&dir));
        mark_registry_joined(&dir).unwrap();
        assert!(is_registry_joined(&dir));
        clear_registry_joined(&dir).unwrap();
        assert!(!is_registry_joined(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
