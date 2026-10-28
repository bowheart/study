//! Documentation served by read_skill / get_app_state, and the user's style library for get_style.

use serde_json::Value;

const FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../docs/SKILL.md")),
    ("pen-schema.md", include_str!("../docs/pen-schema.md")),
    ("execute.md", include_str!("../docs/execute.md")),
    ("components.md", include_str!("../docs/components.md")),
    ("layout.md", include_str!("../docs/layout.md")),
    ("scripts-and-shaders.md", include_str!("../docs/scripts-and-shaders.md")),
];

pub fn read(path: Option<&str>) -> String {
    let want = path.unwrap_or("SKILL.md").trim_start_matches("./");
    let want = want.rsplit('/').next().unwrap_or(want);
    match FILES.iter().find(|(name, _)| name.eq_ignore_ascii_case(want)) {
        Some((_, text)) => text.to_string(),
        None => format!(
            "No skill file '{want}'. Available: {}",
            FILES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// The reference material included with get_app_state.
pub fn app_state_docs() -> String {
    let skill = FILES[0].1.split("---").nth(2).unwrap_or(FILES[0].1).trim();
    format!(
        "# study documentation\n\n{skill}\n\n{}\n\n{}\n\nMore: read_skill({{path: \"components.md\"}}), read_skill({{path: \"layout.md\"}}), read_skill({{path: \"scripts-and-shaders.md\"}}).\n",
        FILES[2].1.trim(),
        FILES[1].1.trim()
    )
}

/// Styles are markdown files in ~/.study/styles. A style may declare required params as
/// lines `@param name: description` and use `{{name}}` placeholders.
pub fn style(name: Option<&str>, params: Option<&Value>) -> String {
    let dir = render::data_dir().join("styles");
    let mut styles: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                .map(|e| (e.path().file_stem().unwrap().to_string_lossy().to_string(), e.path()))
                .collect()
        })
        .unwrap_or_default();
    styles.sort();
    let Some(name) = name else {
        if styles.is_empty() {
            return format!(
                "No styles installed. Styles are markdown files in {} (one per style, optionally with `@param name: description` lines and {{{{name}}}} placeholders). Without a style, choose fonts and colors that fit the brief.",
                dir.display()
            );
        }
        return format!("Available styles:\n{}", styles.iter().map(|(n, _)| format!("- {n}")).collect::<Vec<_>>().join("\n"));
    };
    let Some((_, path)) = styles.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) else {
        return format!("No style named '{name}'. Call get_style() to list styles.");
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let declared: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("@param "))
        .filter_map(|l| l.split_once(':').map(|(a, b)| (a.trim().to_string(), b.trim().to_string())))
        .collect();
    let given = params.and_then(Value::as_object);
    let missing: Vec<&(String, String)> = declared.iter().filter(|(k, _)| given.is_none_or(|g| !g.contains_key(k))).collect();
    if !missing.is_empty() {
        return format!(
            "Style '{name}' needs params. Call get_style again with all of them:\n{}",
            missing.iter().map(|(k, d)| format!("- {k}: {d}")).collect::<Vec<_>>().join("\n")
        );
    }
    let mut out: String = text.lines().filter(|l| !l.trim().starts_with("@param ")).collect::<Vec<_>>().join("\n");
    if let Some(g) = given {
        for (k, v) in g {
            let v = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
            out = out.replace(&format!("{{{{{k}}}}}"), &v);
        }
    }
    out
}
