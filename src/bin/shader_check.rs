use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use naga::valid::{Capabilities, ValidationFlags, Validator};
use wesl::syntax::{ModulePath, PathOrigin};

fn main() {
    let shaders_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/shaders");
    let mut shader_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&shaders_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("wesl")) {
                shader_files.push(path);
            }
        }
    }
    shader_files.sort();

    if shader_files.is_empty() {
        eprintln!("No .wesl shaders found in {}", shaders_dir.display());
        std::process::exit(1);
    }

    let mut resolver = ProjectResolver::new();
    let mut had_error = false;
    let mut validated = 0;

    for path in &shader_files {
        let rel = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(path)
            .display()
            .to_string();
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(e) => {
                eprintln!("[FAIL] {rel}: read error: {e}");
                had_error = true;
                continue;
            }
        };

        let module_path = module_path_for_shader(path);
        resolver.insert(module_path.clone(), source);

        for (label, flags) in permutations_for(path) {
            let name = if label.is_empty() {
                rel.clone()
            } else {
                format!("{rel} ({label})")
            };
            if validate_permutation(&module_path, &name, flags, &resolver) {
                validated += 1;
            } else {
                had_error = true;
            }
        }
    }

    if had_error {
        std::process::exit(1);
    }
    println!("All {validated} shader permutation(s) validated.");
}

fn permutations_for(path: &Path) -> Vec<(&'static str, Vec<(&'static str, bool)>)> {
    let base = vec![("MATERIAL_BIND_GROUP", true)];

    if path.file_stem().and_then(OsStr::to_str) == Some("water") {
        // Water's mesh pipeline supplies vertex colors and may also supply
        // UVs/tangents from generic mesh layouts. Validate each branch that
        // Bevy can actually specialize.
        return vec![
            ("", base.clone()),
            (
                "vertex colors",
                vec![("MATERIAL_BIND_GROUP", true), ("VERTEX_COLORS", true)],
            ),
            (
                "vertex colors, uvs and tangents",
                vec![
                    ("MATERIAL_BIND_GROUP", true),
                    ("VERTEX_COLORS", true),
                    ("VERTEX_UVS_A", true),
                    ("VERTEX_TANGENTS", true),
                ],
            ),
        ];
    }

    if path.file_stem().and_then(OsStr::to_str) == Some("light_shafts") {
        return vec![
            ("simple", base.clone()),
            (
                "detailed",
                vec![("MATERIAL_BIND_GROUP", true), ("DETAILED_SHAFTS", true)],
            ),
            (
                "detailed MSAA",
                vec![
                    ("MATERIAL_BIND_GROUP", true),
                    ("DETAILED_SHAFTS", true),
                    ("MULTISAMPLED_DEPTH", true),
                ],
            ),
        ];
    }

    vec![("", base)]
}

fn validate_permutation(
    module_path: &ModulePath,
    name: &str,
    flags: Vec<(&str, bool)>,
    resolver: &ProjectResolver,
) -> bool {
    let mut options = wesl::CompileOptions {
        imports: true,
        condcomp: true,
        visibility: false,
        ..Default::default()
    };
    for (flag, value) in flags {
        options
            .features
            .flags
            .insert(flag.to_string(), value.into());
    }

    match wesl::compile(module_path, &options, resolver) {
        Ok(compiled) => {
            let wgsl = compiled.to_string();
            match naga::front::wgsl::parse_str(&wgsl) {
                Ok(module) => {
                    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
                    if let Err(e) = validator.validate(&module) {
                        eprintln!("[FAIL] {name}\nNaga WGSL validation error:\n{e:?}");
                        false
                    } else {
                        println!("[OK] {name}");
                        true
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[FAIL] {name}\nNaga WGSL parse error:\n{}",
                        e.emit_to_string(&wgsl)
                    );
                    false
                }
            }
        }
        Err(e) => {
            eprintln!("[FAIL] {name}\n{}", e.diagnostic().render_plain());
            false
        }
    }
}

fn module_path_for_shader(path: &Path) -> ModulePath {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("shader");
    ModulePath {
        origin: PathOrigin::Package("shaders".to_string()),
        components: vec![stem.to_string()],
    }
}

struct ProjectResolver {
    modules: HashMap<ModulePath, String>,
    bevy_pbr_root: Option<PathBuf>,
    bevy_render_root: Option<PathBuf>,
    bevy_core_pipeline_root: Option<PathBuf>,
}

impl ProjectResolver {
    fn new() -> Self {
        Self {
            modules: HashMap::new(),
            bevy_pbr_root: find_crate_root("bevy_pbr-0.20.1"),
            bevy_render_root: find_crate_root("bevy_render-0.20.1"),
            bevy_core_pipeline_root: find_crate_root("bevy_core_pipeline-0.20.1"),
        }
    }

    fn insert(&mut self, path: ModulePath, source: String) {
        self.modules.insert(path, source);
    }

    fn resolve_package(&self, pkg: &str, module_path: &ModulePath) -> Option<String> {
        let root = match pkg {
            "bevy_pbr" => self.bevy_pbr_root.as_ref()?,
            "bevy_render" => self.bevy_render_root.as_ref()?,
            "bevy_core_pipeline" => self.bevy_core_pipeline_root.as_ref()?,
            _ => return None,
        };
        let mut rel = PathBuf::from("src");
        for c in &module_path.components {
            rel.push(c);
        }
        for ext in ["wesl", "wgsl"] {
            let mut p = root.join(&rel);
            p.set_extension(ext);
            if let Ok(s) = std::fs::read_to_string(&p) {
                return Some(s);
            }
        }
        let mut dir = root.join("src");
        for c in &module_path.components {
            dir.push(c);
        }
        for ext in ["wesl", "wgsl"] {
            let p = dir.join(format!("mod.{ext}"));
            if let Ok(s) = std::fs::read_to_string(&p) {
                return Some(s);
            }
        }
        None
    }
}

impl wesl::Resolver for ProjectResolver {
    fn resolve_source<'a>(
        &'a self,
        module_path: &ModulePath,
    ) -> Result<Cow<'a, str>, wesl::error::ResolveError> {
        if module_path.origin == PathOrigin::Package("constants".to_string()) {
            return Ok(Cow::Owned(
                "const MATERIAL_BIND_GROUP: u32 = 3u;\n".to_string(),
            ));
        }
        if let Some(src) = self.modules.get(module_path) {
            return Ok(Cow::Borrowed(src));
        }
        let PathOrigin::Package(pkg) = &module_path.origin else {
            return Err(wesl::error::ResolveError::ModuleNotFound(
                module_path.clone(),
                "non-package module not found".to_string(),
            ));
        };
        if let Some(src) = self.resolve_package(pkg, module_path) {
            return Ok(Cow::Owned(src));
        }
        Err(wesl::error::ResolveError::ModuleNotFound(
            module_path.clone(),
            format!("module not found in project or known Bevy crates (pkg={pkg})"),
        ))
    }

    fn canonical_path(&self, module_path: &ModulePath) -> ModulePath {
        if let PathOrigin::Package(pkg) = &module_path.origin
            && pkg.contains('/')
        {
            return ModulePath {
                origin: PathOrigin::Package(pkg.rsplit('/').next().unwrap().to_string()),
                components: module_path.components.clone(),
            };
        }
        module_path.clone()
    }
}

fn find_crate_root(prefix: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let registry = PathBuf::from(home).join(".cargo/registry/src");
    for entry in std::fs::read_dir(registry).ok()? {
        let index_dir = entry.ok()?.path();
        if let Ok(crates) = std::fs::read_dir(&index_dir) {
            for c in crates.flatten() {
                let name = c.file_name().to_string_lossy().to_string();
                if name.starts_with(prefix) {
                    return Some(c.path());
                }
            }
        }
    }
    None
}
