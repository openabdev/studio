//! Bundle composition — the **compose** seam of the agent-deployment ADR (slice 1).
//!
//! An agent's identity is **plain files** ([`Bundle`]): `config.toml`, persona
//! (`CLAUDE.md`, `agent_profiling/…`), and skills (`.claude/skills/…`). Nothing
//! here compiles or touches a runtime — this crate turns an authored
//! **[`Template`] ⊕ [`Overlay`]** (plus a shared [`SkillsLibrary`]) into one
//! deterministic `{path → bytes}` bundle + an image tag. The provider drivers
//! (slice 2+: push to S3 state / ConfigMap) consume a [`Bundle`]; they are the
//! only layer that knows the carrier. Keeping compose pure is what lets the whole
//! slice be verified with `cargo test` — no AWS, no bundle-macos.
//!
//! ## Layers & precedence (last-writer-wins per path)
//!
//! Later layers overwrite earlier ones at the same path — the "Helm chart +
//! values" model the ADR calls for:
//!
//! 1. **template files** — the golden base bundle.
//! 2. **skills**, resolved by reference from the library into
//!    `.claude/skills/<name>/…`. Template-referenced skills first, then
//!    overlay-referenced ones (an overlay re-reference is a no-op — same files).
//! 3. **overlay files** — the per-agent specifics, highest precedence.
//!
//! The image tag is `overlay.image_tag`, falling back to `template.image_tag`.
//!
//! ## Text in, bytes out
//!
//! Authored inputs are UTF-8 text (config/persona/skills all are), so the editor
//! surface stays simple; the composed [`Bundle`] holds `Vec<u8>` because the ADR's
//! transport contract is `{path → bytes}` and the drivers write raw bytes. Binary
//! authored assets are out of scope for this slice.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Directory prefix a referenced skill's files are placed under in the bundle.
/// A skill named `foo` with file `SKILL.md` lands at `.claude/skills/foo/SKILL.md`.
const SKILLS_PREFIX: &str = ".claude/skills";

/// A reusable base bundle — the "golden bundle" an operator authors once and
/// reuses across agents. `BTreeMap` keeps files ordered so composition (and the
/// resulting digest) is deterministic regardless of insert order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    /// Default image tag for agents built from this template. May be overridden
    /// per agent by [`Overlay::image_tag`].
    #[serde(default)]
    pub image_tag: String,
    /// Base files keyed by bundle-relative path (e.g. `config.toml`, `CLAUDE.md`).
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Skills pulled from the shared library by name (attached by reference).
    #[serde(default)]
    pub skills: Vec<String>,
}

/// Per-agent specifics layered on a template. Everything is optional: an empty
/// overlay composes to exactly the template.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlay {
    pub name: String,
    /// Overrides the template's default image tag when set + non-empty.
    #[serde(default)]
    pub image_tag: Option<String>,
    /// Per-agent files; override template/skill files at the same path.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Extra skills attached to this agent, by reference.
    #[serde(default)]
    pub skills: Vec<String>,
}

/// A single reusable skill: the files that live under its
/// `.claude/skills/<name>/` directory in a composed bundle.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Files keyed by path **relative to the skill's own directory** (e.g.
    /// `SKILL.md`, `scripts/run.sh`) — never an absolute or `.claude/…` path.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// The shared, author-once skills library referenced from templates/overlays.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsLibrary {
    #[serde(default)]
    pub skills: BTreeMap<String, Skill>,
}

impl SkillsLibrary {
    /// Build a library from `(name, skill)` pairs — convenience for callers/tests.
    pub fn from_iter<I, N>(it: I) -> Self
    where
        I: IntoIterator<Item = (N, Skill)>,
        N: Into<String>,
    {
        SkillsLibrary {
            skills: it.into_iter().map(|(n, s)| (n.into(), s)).collect(),
        }
    }
}

/// The composed, deterministic result: one image tag + a `{path → bytes}` file
/// map, ready for a provider driver to land on a runtime's file carrier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub image_tag: String,
    /// Sorted (`BTreeMap`) so iteration order — and thus [`Bundle::digest`] — is
    /// stable across composes of equal inputs.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Bundle {
    /// Bundle-relative paths, sorted.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// A content-address for the bundle: `sha256:<hex>` over the image tag and
    /// every `(path, bytes)` in sorted order, length-prefixed so no
    /// concatenation collision is possible. This is the stable identity the
    /// desired-state record (slice 2 / fleet-store #18) keys on: equal inputs →
    /// equal digest, and any change to a path or a byte changes it.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        // Domain-separate + length-prefix every field so distinct structures can't
        // hash to the same stream (e.g. tag "ab"+path "c" vs tag "a"+path "bc").
        h.update((self.image_tag.len() as u64).to_le_bytes());
        h.update(self.image_tag.as_bytes());
        h.update((self.files.len() as u64).to_le_bytes());
        for (path, bytes) in &self.files {
            h.update((path.len() as u64).to_le_bytes());
            h.update(path.as_bytes());
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(bytes);
        }
        format!("sha256:{:x}", h.finalize())
    }
}

/// A UTF-8-lossy, serde-friendly view of one bundle file for the preview UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePreview {
    pub path: String,
    /// Content decoded as UTF-8 (lossy). Authored files are always valid UTF-8;
    /// `binary` flags the rare case a byte sequence wasn't, so the UI can say so
    /// instead of showing replacement characters as if they were real content.
    pub text: String,
    pub bytes: usize,
    pub binary: bool,
}

/// Everything the compose-preview panel renders without holding raw bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundlePreview {
    pub image_tag: String,
    pub digest: String,
    pub files: Vec<FilePreview>,
}

impl Bundle {
    /// A serde-friendly, text-oriented view for the UI — paths in sorted order,
    /// content decoded UTF-8-lossy, plus the image tag and digest.
    pub fn preview(&self) -> BundlePreview {
        let files = self
            .files
            .iter()
            .map(|(path, bytes)| {
                let text = String::from_utf8_lossy(bytes);
                FilePreview {
                    path: path.clone(),
                    binary: text.as_bytes() != bytes.as_slice(),
                    bytes: bytes.len(),
                    text: text.into_owned(),
                }
            })
            .collect();
        BundlePreview {
            image_tag: self.image_tag.clone(),
            digest: self.digest(),
            files,
        }
    }
}

/// The authored library the operator edits in Studio: named templates + overlays
/// over a shared skills library. Persisted as one JSON document (like Studio's
/// other in-app config surfaces) and the input to [`compose_named`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub templates: BTreeMap<String, Template>,
    #[serde(default)]
    pub overlays: BTreeMap<String, Overlay>,
    #[serde(default)]
    pub skills: SkillsLibrary,
}

/// Compose a named `template ⊕ overlay` out of a [`Library`]. `overlay` is
/// optional — `None` composes the bare template (empty overlay).
pub fn compose_named(
    library: &Library,
    template: &str,
    overlay: Option<&str>,
) -> Result<Bundle, ComposeError> {
    let tmpl = library
        .templates
        .get(template)
        .ok_or_else(|| ComposeError::UnknownTemplate {
            template: template.to_string(),
        })?;
    let empty;
    let ovl = match overlay {
        Some(name) => library
            .overlays
            .get(name)
            .ok_or_else(|| ComposeError::UnknownOverlay {
                overlay: name.to_string(),
            })?,
        None => {
            empty = Overlay::default();
            &empty
        }
    };
    compose(tmpl, ovl, &library.skills)
}

/// Why a compose failed. Compose is total apart from these authoring mistakes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeError {
    /// [`compose_named`] was given a template name absent from the library.
    UnknownTemplate { template: String },
    /// [`compose_named`] was given an overlay name absent from the library.
    UnknownOverlay { overlay: String },
    /// A template/overlay referenced a skill name absent from the library.
    /// `referenced_by` records where, to point the operator at the right layer.
    UnknownSkill {
        skill: String,
        referenced_by: SkillRef,
    },
    /// Neither the overlay nor the template supplied a non-empty image tag — the
    /// composed agent has nothing to run.
    MissingImageTag,
}

/// Which layer referenced a skill — for actionable [`ComposeError::UnknownSkill`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillRef {
    Template,
    Overlay,
}

impl fmt::Display for ComposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComposeError::UnknownTemplate { template } => {
                write!(f, "no template named \"{template}\" in the library")
            }
            ComposeError::UnknownOverlay { overlay } => {
                write!(f, "no overlay named \"{overlay}\" in the library")
            }
            ComposeError::UnknownSkill {
                skill,
                referenced_by,
            } => {
                let layer = match referenced_by {
                    SkillRef::Template => "template",
                    SkillRef::Overlay => "overlay",
                };
                write!(
                    f,
                    "{layer} references skill \"{skill}\" which is not in the skills library"
                )
            }
            ComposeError::MissingImageTag => write!(
                f,
                "no image tag: neither the overlay nor the template set a non-empty image_tag"
            ),
        }
    }
}

impl std::error::Error for ComposeError {}

/// Compose a concrete agent bundle from `template ⊕ overlay`, resolving skills by
/// reference from `library`. Deterministic and pure; see the module docs for the
/// precedence order.
pub fn compose(
    template: &Template,
    overlay: &Overlay,
    library: &SkillsLibrary,
) -> Result<Bundle, ComposeError> {
    // Image tag: overlay override (if non-empty) wins, else the template default.
    let image_tag = overlay
        .image_tag
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(template.image_tag.as_str())
        .to_string();
    if image_tag.is_empty() {
        return Err(ComposeError::MissingImageTag);
    }

    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    // Layer 1: template base files.
    for (path, content) in &template.files {
        files.insert(path.clone(), content.clone().into_bytes());
    }

    // Layer 2: skills by reference. Template-referenced first, then overlay's, so
    // an overlay that re-lists a template skill is a harmless no-op (identical
    // files) and a genuinely new skill layers on top. Unknown names are errors,
    // attributed to the layer that referenced them.
    for skill in &template.skills {
        resolve_skill(skill, SkillRef::Template, library, &mut files)?;
    }
    for skill in &overlay.skills {
        resolve_skill(skill, SkillRef::Overlay, library, &mut files)?;
    }

    // Layer 3: overlay files — highest precedence, override anything above.
    for (path, content) in &overlay.files {
        files.insert(path.clone(), content.clone().into_bytes());
    }

    Ok(Bundle { image_tag, files })
}

/// Expand one referenced skill's files under `.claude/skills/<name>/…` into the
/// accumulating bundle (last-writer-wins, like every other layer).
fn resolve_skill(
    name: &str,
    referenced_by: SkillRef,
    library: &SkillsLibrary,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), ComposeError> {
    let skill = library
        .skills
        .get(name)
        .ok_or_else(|| ComposeError::UnknownSkill {
            skill: name.to_string(),
            referenced_by,
        })?;
    for (rel, content) in &skill.files {
        let path = format!("{SKILLS_PREFIX}/{name}/{rel}");
        files.insert(path, content.clone().into_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpl() -> Template {
        Template {
            name: "base".into(),
            image_tag: "ghcr.io/openabdev/openab:0.9.0-claude".into(),
            files: BTreeMap::from([
                ("config.toml".into(), "[agent]\nname = \"base\"\n".into()),
                ("CLAUDE.md".into(), "# base persona\n".into()),
            ]),
            skills: vec![],
        }
    }

    fn skill_with(files: &[(&str, &str)]) -> Skill {
        Skill {
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn empty_overlay_composes_to_template() {
        let b = compose(&tmpl(), &Overlay::default(), &SkillsLibrary::default()).unwrap();
        assert_eq!(b.image_tag, "ghcr.io/openabdev/openab:0.9.0-claude");
        assert_eq!(
            b.paths().collect::<Vec<_>>(),
            vec!["CLAUDE.md", "config.toml"]
        );
        assert_eq!(b.files["CLAUDE.md"], b"# base persona\n");
    }

    #[test]
    fn overlay_file_overrides_template_last_writer_wins() {
        let overlay = Overlay {
            name: "orca".into(),
            files: BTreeMap::from([("CLAUDE.md".into(), "# orca persona\n".into())]),
            ..Default::default()
        };
        let b = compose(&tmpl(), &overlay, &SkillsLibrary::default()).unwrap();
        assert_eq!(b.files["CLAUDE.md"], b"# orca persona\n");
        // untouched template file survives
        assert_eq!(b.files["config.toml"], b"[agent]\nname = \"base\"\n");
    }

    #[test]
    fn overlay_image_tag_overrides_template() {
        let overlay = Overlay {
            image_tag: Some("ghcr.io/openabdev/openab:edge".into()),
            ..Default::default()
        };
        let b = compose(&tmpl(), &overlay, &SkillsLibrary::default()).unwrap();
        assert_eq!(b.image_tag, "ghcr.io/openabdev/openab:edge");
    }

    #[test]
    fn empty_overlay_image_tag_falls_back_to_template() {
        let overlay = Overlay {
            image_tag: Some(String::new()),
            ..Default::default()
        };
        let b = compose(&tmpl(), &overlay, &SkillsLibrary::default()).unwrap();
        assert_eq!(b.image_tag, "ghcr.io/openabdev/openab:0.9.0-claude");
    }

    #[test]
    fn skills_resolve_by_reference_under_prefix() {
        let lib = SkillsLibrary::from_iter([(
            "memory",
            skill_with(&[("SKILL.md", "# memory\n"), ("scripts/run.sh", "echo hi\n")]),
        )]);
        let mut t = tmpl();
        t.skills = vec!["memory".into()];
        let b = compose(&t, &Overlay::default(), &lib).unwrap();
        assert_eq!(b.files[".claude/skills/memory/SKILL.md"], b"# memory\n");
        assert_eq!(
            b.files[".claude/skills/memory/scripts/run.sh"],
            b"echo hi\n"
        );
        // base files still present alongside the skill
        assert!(b.files.contains_key("config.toml"));
    }

    #[test]
    fn overlay_skills_add_to_template_skills() {
        let lib = SkillsLibrary::from_iter([
            ("a", skill_with(&[("SKILL.md", "a\n")])),
            ("b", skill_with(&[("SKILL.md", "b\n")])),
        ]);
        let mut t = tmpl();
        t.skills = vec!["a".into()];
        let overlay = Overlay {
            skills: vec!["b".into()],
            ..Default::default()
        };
        let b = compose(&t, &overlay, &lib).unwrap();
        assert_eq!(b.files[".claude/skills/a/SKILL.md"], b"a\n");
        assert_eq!(b.files[".claude/skills/b/SKILL.md"], b"b\n");
    }

    #[test]
    fn overlay_file_overrides_skill_file() {
        let lib = SkillsLibrary::from_iter([("s", skill_with(&[("SKILL.md", "from skill\n")]))]);
        let mut t = tmpl();
        t.skills = vec!["s".into()];
        let overlay = Overlay {
            files: BTreeMap::from([(
                ".claude/skills/s/SKILL.md".into(),
                "from overlay\n".into(),
            )]),
            ..Default::default()
        };
        let b = compose(&t, &overlay, &lib).unwrap();
        assert_eq!(b.files[".claude/skills/s/SKILL.md"], b"from overlay\n");
    }

    #[test]
    fn unknown_template_skill_errors_attributed_to_template() {
        let mut t = tmpl();
        t.skills = vec!["nope".into()];
        let err = compose(&t, &Overlay::default(), &SkillsLibrary::default()).unwrap_err();
        assert_eq!(
            err,
            ComposeError::UnknownSkill {
                skill: "nope".into(),
                referenced_by: SkillRef::Template,
            }
        );
        assert!(err.to_string().contains("template references skill \"nope\""));
    }

    #[test]
    fn unknown_overlay_skill_errors_attributed_to_overlay() {
        let overlay = Overlay {
            skills: vec!["ghost".into()],
            ..Default::default()
        };
        let err = compose(&tmpl(), &overlay, &SkillsLibrary::default()).unwrap_err();
        assert_eq!(
            err,
            ComposeError::UnknownSkill {
                skill: "ghost".into(),
                referenced_by: SkillRef::Overlay,
            }
        );
    }

    #[test]
    fn missing_image_tag_errors() {
        let t = Template {
            name: "n".into(),
            image_tag: String::new(),
            ..Default::default()
        };
        let err = compose(&t, &Overlay::default(), &SkillsLibrary::default()).unwrap_err();
        assert_eq!(err, ComposeError::MissingImageTag);
    }

    #[test]
    fn digest_is_stable_and_order_independent() {
        // Two templates with the same files inserted in different orders must
        // produce byte-identical bundles and digests (BTreeMap canonicalises).
        let a = Template {
            name: "a".into(),
            image_tag: "t".into(),
            files: BTreeMap::from([("x".into(), "1".into()), ("y".into(), "2".into())]),
            skills: vec![],
        };
        let mut b_files = BTreeMap::new();
        b_files.insert("y".to_string(), "2".to_string());
        b_files.insert("x".to_string(), "1".to_string());
        let b = Template {
            name: "b".into(),
            image_tag: "t".into(),
            files: b_files,
            skills: vec![],
        };
        let ba = compose(&a, &Overlay::default(), &SkillsLibrary::default()).unwrap();
        let bb = compose(&b, &Overlay::default(), &SkillsLibrary::default()).unwrap();
        assert_eq!(ba.digest(), bb.digest());
        assert!(ba.digest().starts_with("sha256:"));
    }

    #[test]
    fn digest_changes_when_a_byte_changes() {
        let base = compose(&tmpl(), &Overlay::default(), &SkillsLibrary::default()).unwrap();
        let mut t = tmpl();
        t.files.insert("config.toml".into(), "[agent]\nname = \"x\"\n".into());
        let changed = compose(&t, &Overlay::default(), &SkillsLibrary::default()).unwrap();
        assert_ne!(base.digest(), changed.digest());
    }

    #[test]
    fn digest_no_field_boundary_collision() {
        // image_tag "ab" + path "c" must not collide with image_tag "a" + path "bc".
        let t1 = Template {
            name: "n".into(),
            image_tag: "ab".into(),
            files: BTreeMap::from([("c".into(), String::new())]),
            skills: vec![],
        };
        let t2 = Template {
            name: "n".into(),
            image_tag: "a".into(),
            files: BTreeMap::from([("bc".into(), String::new())]),
            skills: vec![],
        };
        let b1 = compose(&t1, &Overlay::default(), &SkillsLibrary::default()).unwrap();
        let b2 = compose(&t2, &Overlay::default(), &SkillsLibrary::default()).unwrap();
        assert_ne!(b1.digest(), b2.digest());
    }

    #[test]
    fn compose_named_resolves_template_and_overlay() {
        let lib = Library {
            templates: BTreeMap::from([("base".into(), tmpl())]),
            overlays: BTreeMap::from([(
                "orca".into(),
                Overlay {
                    name: "orca".into(),
                    files: BTreeMap::from([("CLAUDE.md".into(), "# orca\n".into())]),
                    ..Default::default()
                },
            )]),
            skills: SkillsLibrary::default(),
        };
        let b = compose_named(&lib, "base", Some("orca")).unwrap();
        assert_eq!(b.files["CLAUDE.md"], b"# orca\n");
        // None overlay composes the bare template
        let bare = compose_named(&lib, "base", None).unwrap();
        assert_eq!(bare.files["CLAUDE.md"], b"# base persona\n");
    }

    #[test]
    fn compose_named_unknown_names_error() {
        let lib = Library {
            templates: BTreeMap::from([("base".into(), tmpl())]),
            ..Default::default()
        };
        assert_eq!(
            compose_named(&lib, "ghost", None).unwrap_err(),
            ComposeError::UnknownTemplate {
                template: "ghost".into()
            }
        );
        assert_eq!(
            compose_named(&lib, "base", Some("ghost")).unwrap_err(),
            ComposeError::UnknownOverlay {
                overlay: "ghost".into()
            }
        );
    }

    #[test]
    fn preview_is_sorted_text_with_digest() {
        let lib = SkillsLibrary::from_iter([("s", skill_with(&[("SKILL.md", "hi\n")]))]);
        let mut t = tmpl();
        t.skills = vec!["s".into()];
        let p = compose(&t, &Overlay::default(), &lib).unwrap().preview();
        assert_eq!(p.image_tag, "ghcr.io/openabdev/openab:0.9.0-claude");
        assert!(p.digest.starts_with("sha256:"));
        let paths: Vec<_> = p.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![".claude/skills/s/SKILL.md", "CLAUDE.md", "config.toml"]
        );
        let claude = p.files.iter().find(|f| f.path == "CLAUDE.md").unwrap();
        assert_eq!(claude.text, "# base persona\n");
        assert!(!claude.binary);
    }

    #[test]
    fn round_trips_through_json() {
        // The Tauri boundary shuttles these as JSON; make sure serde is wired.
        let t = tmpl();
        let s = serde_json::to_string(&t).unwrap();
        let back: Template = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }
}
