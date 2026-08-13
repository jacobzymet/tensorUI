use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const MAX_SKILLS: usize = 32;
const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION_LEN: usize = 280;
const MAX_CONTENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_filename: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserSkillPublic {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub content: String,
    pub source_filename: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub content_chars: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillUpsert {
    pub name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub enabled: Option<bool>,
    pub source_filename: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SkillIndex {
    skills: Vec<SkillMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillMeta {
    id: String,
    name: String,
    description: String,
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_filename: Option<String>,
    created_at: u64,
    updated_at: u64,
}

pub struct SkillStore {
    root: PathBuf,
}

impl SkillStore {
    pub fn new(config_path: &Path) -> Self {
        let root = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("chat-skills");
        Self { root }
    }

    pub fn list(&self) -> Result<Vec<UserSkill>> {
        let index = self.load_index()?;
        let mut out = Vec::with_capacity(index.skills.len());
        for meta in index.skills {
            let content = self.read_content(&meta.id).unwrap_or_default();
            out.push(UserSkill {
                id: meta.id,
                name: meta.name,
                description: meta.description,
                enabled: meta.enabled,
                content,
                source_filename: meta.source_filename,
                created_at: meta.created_at,
                updated_at: meta.updated_at,
            });
        }
        Ok(out)
    }

    pub fn enabled_skills(&self) -> Result<Vec<UserSkill>> {
        Ok(self.list()?.into_iter().filter(|s| s.enabled).collect())
    }

    pub fn create(&self, upsert: SkillUpsert) -> Result<UserSkill> {
        let mut index = self.load_index()?;
        if index.skills.len() >= MAX_SKILLS {
            bail!("At most {MAX_SKILLS} skills are allowed.");
        }
        let now = unix_now();
        let id = generate_id();
        let name = sanitize_name(upsert.name.as_deref().unwrap_or("Untitled skill"))?;
        let description = sanitize_description(upsert.description.as_deref().unwrap_or(""))?;
        let content = sanitize_content(upsert.content.as_deref().unwrap_or(""))?;
        let skill = UserSkill {
            id: id.clone(),
            name: name.clone(),
            description: description.clone(),
            enabled: upsert.enabled.unwrap_or(true),
            content: content.clone(),
            source_filename: sanitize_filename(upsert.source_filename),
            created_at: now,
            updated_at: now,
        };
        self.write_content(&id, &content)?;
        index.skills.push(SkillMeta {
            id,
            name,
            description,
            enabled: skill.enabled,
            source_filename: skill.source_filename.clone(),
            created_at: now,
            updated_at: now,
        });
        self.save_index(&index)?;
        Ok(skill)
    }

    pub fn update(&self, id: &str, upsert: SkillUpsert) -> Result<UserSkill> {
        let mut index = self.load_index()?;
        let meta = index
            .skills
            .iter_mut()
            .find(|s| s.id == id)
            .with_context(|| format!("Unknown skill id: {id}"))?;
        if let Some(name) = upsert.name.as_deref() {
            meta.name = sanitize_name(name)?;
        }
        if let Some(description) = upsert.description.as_deref() {
            meta.description = sanitize_description(description)?;
        }
        if let Some(enabled) = upsert.enabled {
            meta.enabled = enabled;
        }
        if upsert.source_filename.is_some() {
            meta.source_filename = sanitize_filename(upsert.source_filename);
        }
        let mut content = self.read_content(id).unwrap_or_default();
        if let Some(raw) = upsert.content.as_deref() {
            content = sanitize_content(raw)?;
            self.write_content(id, &content)?;
        }
        meta.updated_at = unix_now();
        let skill = UserSkill {
            id: meta.id.clone(),
            name: meta.name.clone(),
            description: meta.description.clone(),
            enabled: meta.enabled,
            content,
            source_filename: meta.source_filename.clone(),
            created_at: meta.created_at,
            updated_at: meta.updated_at,
        };
        self.save_index(&index)?;
        Ok(skill)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let mut index = self.load_index()?;
        let before = index.skills.len();
        index.skills.retain(|s| s.id != id);
        if index.skills.len() == before {
            bail!("Unknown skill id: {id}");
        }
        self.save_index(&index)?;
        let _ = fs::remove_file(self.content_path(id));
        Ok(())
    }

    pub fn import_markdown(&self, filename: Option<&str>, raw: &str) -> Result<UserSkill> {
        let parsed = parse_skill_markdown(raw);
        let fallback_name = filename
            .and_then(|name| {
                Path::new(name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.replace(['_', '-'], " "))
            })
            .unwrap_or_else(|| "Imported skill".into());
        self.create(SkillUpsert {
            name: Some(parsed.name.unwrap_or(fallback_name)),
            description: Some(parsed.description.unwrap_or_default()),
            content: Some(parsed.content),
            enabled: Some(true),
            source_filename: filename.map(str::to_string),
        })
    }

    fn load_index(&self) -> Result<SkillIndex> {
        let path = self.index_path();
        if !path.is_file() {
            return Ok(SkillIndex::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("invalid skill index {}", path.display()))
    }

    fn save_index(&self, index: &SkillIndex) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("could not create {}", self.root.display()))?;
        let path = self.index_path();
        let raw = serde_json::to_string_pretty(index).context("serialize skill index")?;
        fs::write(&path, raw).with_context(|| format!("could not write {}", path.display()))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    fn content_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.md"))
    }

    fn read_content(&self, id: &str) -> Result<String> {
        let path = self.content_path(id);
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))
    }

    fn write_content(&self, id: &str, content: &str) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("could not create {}", self.root.display()))?;
        let path = self.content_path(id);
        fs::write(&path, content).with_context(|| format!("could not write {}", path.display()))
    }
}

impl UserSkill {
    pub fn to_public(&self) -> UserSkillPublic {
        UserSkillPublic {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            enabled: self.enabled,
            content: self.content.clone(),
            source_filename: self.source_filename.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            content_chars: self.content.chars().count(),
        }
    }

    pub fn catalog_line(&self) -> String {
        let desc = self.description.trim();
        if desc.is_empty() {
            format!("- {} (id: {})", self.name, self.id)
        } else {
            format!("- {} (id: {}): {desc}", self.name, self.id)
        }
    }

    pub fn full_instructions(&self) -> String {
        let mut out = format!("# Skill: {}\n", self.name);
        if !self.description.trim().is_empty() {
            out.push_str(&format!("\n{}\n", self.description.trim()));
        }
        out.push('\n');
        out.push_str(self.content.trim());
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

struct ParsedMarkdown {
    name: Option<String>,
    description: Option<String>,
    content: String,
}

fn parse_skill_markdown(raw: &str) -> ParsedMarkdown {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        let front = &rest[..end];
        let body = rest[end + 4..].trim_start_matches('\n').to_string();
        let mut name = None;
        let mut description = None;
        for line in front.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("name:") {
                name = Some(value.trim().trim_matches('"').to_string());
            } else if let Some(value) = line.strip_prefix("description:") {
                description = Some(value.trim().trim_matches('"').to_string());
            }
        }
        return ParsedMarkdown {
            name,
            description,
            content: body,
        };
    }
    ParsedMarkdown {
        name: None,
        description: None,
        content: trimmed.to_string(),
    }
}

fn sanitize_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        bail!("Skill name cannot be empty.");
    }
    if name.chars().count() > MAX_NAME_LEN {
        bail!("Skill name must be at most {MAX_NAME_LEN} characters.");
    }
    Ok(name.to_string())
}

fn sanitize_description(raw: &str) -> Result<String> {
    let description = raw.trim();
    if description.chars().count() > MAX_DESCRIPTION_LEN {
        bail!("Skill description must be at most {MAX_DESCRIPTION_LEN} characters.");
    }
    Ok(description.to_string())
}

fn sanitize_content(raw: &str) -> Result<String> {
    if raw.len() > MAX_CONTENT_BYTES {
        bail!("Skill content must be at most {MAX_CONTENT_BYTES} bytes.");
    }
    Ok(raw.to_string())
}

fn sanitize_filename(raw: Option<String>) -> Option<String> {
    raw.map(|name| {
        Path::new(name.trim())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("skill.md")
            .chars()
            .take(120)
            .collect()
    })
    .filter(|s: &String| !s.is_empty())
}

fn generate_id() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    format!(
        "sk-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn user_skills_catalog_block(skills: &[UserSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut lines =
        vec![crate::prompts::trim_prompt(crate::prompts::agent::SKILLS_CATALOG_INTRO).to_string()];
    for skill in skills {
        lines.push(skill.catalog_line());
    }
    lines.push(String::new());
    lines.push(
        crate::prompts::trim_prompt(crate::prompts::agent::SKILLS_CATALOG_FOOTER).to_string(),
    );
    lines.join("\n")
}

pub fn find_skill<'a>(skills: &'a [UserSkill], key: &str) -> Option<&'a UserSkill> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    skills
        .iter()
        .find(|skill| skill.id.eq_ignore_ascii_case(key) || skill.name.eq_ignore_ascii_case(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_parses_frontmatter_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let store = SkillStore::new(&config);
        let skill = store
            .import_markdown(
                Some("code-review.md"),
                "---\nname: Code review\ndescription: Review diffs carefully\n---\n\n# Rules\nBe concise.\n",
            )
            .unwrap();
        assert_eq!(skill.name, "Code review");
        assert_eq!(skill.description, "Review diffs carefully");
        assert!(skill.content.contains("Be concise"));
        assert_eq!(skill.source_filename.as_deref(), Some("code-review.md"));
        assert!(skill.enabled);

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, skill.id);

        let catalog = user_skills_catalog_block(&listed);
        assert!(catalog.contains("available skills:"));
        assert!(catalog.contains("Code review"));
        assert!(catalog.contains("Review diffs carefully"));
        assert!(!catalog.contains("Be concise"));
        assert!(listed[0].full_instructions().contains("Be concise"));
    }

    #[test]
    fn update_and_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&dir.path().join("config.toml"));
        let created = store
            .create(SkillUpsert {
                name: Some("Draft".into()),
                description: Some("desc".into()),
                content: Some("body".into()),
                enabled: Some(true),
                source_filename: None,
            })
            .unwrap();
        let updated = store
            .update(
                &created.id,
                SkillUpsert {
                    name: Some("Renamed".into()),
                    description: None,
                    content: Some("new body".into()),
                    enabled: Some(false),
                    source_filename: None,
                },
            )
            .unwrap();
        assert_eq!(updated.name, "Renamed");
        assert!(!updated.enabled);
        assert_eq!(updated.content, "new body");
        store.delete(&created.id).unwrap();
        assert!(store.list().unwrap().is_empty());
    }
}
