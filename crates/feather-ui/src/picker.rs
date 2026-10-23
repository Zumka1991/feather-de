//! Bounded IPC between a requester and the native Files picker. Call IO on workers.
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    process::{Command, Stdio},
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Open,
    Save,
    Directory,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub name: String,
    pub rules: Vec<(u32, String)>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub options: Vec<(String, String)>,
    pub selected: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub title: String,
    pub accept: String,
    pub mode: Mode,
    pub multiple: bool,
    pub start: Option<Vec<u8>>,
    pub name: String,
    pub filters: Vec<Filter>,
    pub filter: usize,
    pub choices: Vec<Choice>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub paths: Vec<Vec<u8>>,
    pub filter: usize,
    pub choices: Vec<(String, String)>,
}
impl Selection {
    pub fn local_paths(&self) -> Result<Vec<PathBuf>, String> {
        if self.paths.is_empty() || self.paths.len() > 64 {
            return Err("invalid selection length".into());
        }
        self.paths
            .iter()
            .map(|p| {
                let path = PathBuf::from(std::ffi::OsString::from_vec(p.clone()));
                if !path.is_absolute() || p.contains(&0) || p.len() > 16384 {
                    Err("invalid local path".into())
                } else {
                    Ok(path)
                }
            })
            .collect()
    }
}
impl Config {
    pub fn validate(&mut self) -> Result<(), String> {
        if self.title.len() > 1024
            || self.accept.len() > 256
            || self.name.len() > 1024
            || self.filters.len() > 32
            || self.choices.len() > 4
            || self.filters.iter().any(|f| {
                f.name.len() > 256
                    || f.rules.len() > 64
                    || f.rules
                        .iter()
                        .any(|(kind, p)| *kind > 1 || p.len() > 256 || p.contains('\0'))
            })
            || self.choices.iter().any(|c| {
                c.id.len() > 256
                    || c.label.len() > 256
                    || c.options.len() > 32
                    || c.selected.len() > 256
                    || c.options
                        .iter()
                        .any(|(id, label)| id.len() > 256 || label.len() > 256)
            })
        {
            return Err("Слишком большой запрос выбора файла".into());
        }
        if let Some(start) = &self.start {
            if start.len() > 16384 || start.contains(&0) || !start.starts_with(b"/") {
                return Err("Неверный путь".into());
            }
        }
        if self.mode == Mode::Save {
            self.multiple = false;
        }
        self.filter = self.filter.min(self.filters.len());
        if self.title.is_empty() {
            self.title = if self.mode == Mode::Save {
                "Сохранить файл"
            } else {
                "Выберите файл"
            }
            .into();
        }
        if self.accept.is_empty() {
            self.accept = match self.mode {
                Mode::Save => "Сохранить",
                Mode::Directory => "Выбрать папку",
                Mode::Open => "Открыть",
            }
            .into();
        }
        for choice in &mut self.choices {
            if choice.options.is_empty() {
                if choice.selected != "true" {
                    choice.selected = "false".into();
                }
            } else if !choice.options.iter().any(|(id, _)| *id == choice.selected) {
                choice.selected = choice.options[0].0.clone();
            }
        }
        Ok(())
    }
    pub fn matches(&self, path: &std::path::Path, filter: usize) -> bool {
        let Some(filter) = self.filters.get(filter) else {
            return true;
        };
        let Some(name) = path.file_name() else {
            return false;
        };
        filter.rules.iter().any(|(kind, rule)| {
            if *kind == 0 {
                match (
                    std::ffi::CString::new(rule.as_bytes()),
                    std::ffi::CString::new(name.as_bytes()),
                ) {
                    (Ok(pattern), Ok(name)) => unsafe {
                        libc::fnmatch(pattern.as_ptr(), name.as_ptr(), libc::FNM_CASEFOLD) == 0
                    },
                    _ => false,
                }
            } else {
                mime_guess::from_path(path).iter().any(|m| {
                    m.essence_str() == rule
                        || rule
                            .strip_suffix("/*")
                            .is_some_and(|p| m.type_().as_str() == p)
                })
            }
        })
    }
}
pub fn read_config() -> Result<Config, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("picker request too large".into());
    }
    let mut config: Config = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    config.validate()?;
    Ok(config)
}
pub fn write_selection(selection: &Selection) -> Result<(), String> {
    let data = serde_json::to_vec(selection).map_err(|e| e.to_string())?;
    std::io::stdout()
        .lock()
        .write_all(&data)
        .map_err(|e| e.to_string())
}
pub fn run(
    config: &Config,
    socket: Option<&str>,
    spawned: impl FnOnce(u32),
) -> Result<Option<Selection>, String> {
    let mut config = config.clone();
    config.validate()?;
    let data = serde_json::to_vec(&config).map_err(|e| e.to_string())?;
    if data.len() > 65536 {
        return Err("picker request too large".into());
    }
    let own = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("feather-files")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "feather-files".into());
    let mut command = Command::new(own);
    command
        .arg("--file-picker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(socket) = socket {
        command.env("WAYLAND_DISPLAY", socket);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Не удалось открыть Файлы: {e}"))?;
    spawned(child.id());
    let write = child.stdin.take().unwrap().write_all(&data);
    if write.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Не удалось передать запрос Файлам".into());
    }
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut output);
    if output.len() > 2 * 1024 * 1024 {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if read.is_err() || !status.success() || output.len() > 2 * 1024 * 1024 {
        return Err("Диалог выбора файла завершился с ошибкой".into());
    }
    if output.is_empty() {
        return Ok(None);
    }
    let selection: Selection = serde_json::from_slice(&output).map_err(|e| e.to_string())?;
    selection.local_paths()?;
    Ok(Some(selection))
}
pub fn path_bytes(path: &std::path::Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_raw_paths_and_limits() {
        let mut config = Config {
            filters: vec![
                Filter {
                    name: "Images".into(),
                    rules: vec![(1, "image/*".into())],
                },
                Filter {
                    name: "PNG".into(),
                    rules: vec![(0, "*.png".into())],
                },
            ],
            ..Default::default()
        };
        config.validate().unwrap();
        assert!(config.matches(std::path::Path::new("/tmp/a.JPG"), 0));
        assert!(config.matches(std::path::Path::new("/tmp/a.PNG"), 1));
        assert!(!config.matches(std::path::Path::new("/tmp/a.pdf"), 0));
        let selection = Selection {
            paths: vec![b"/tmp/a\n\xff.png".to_vec()],
            ..Default::default()
        };
        assert_eq!(
            path_bytes(&selection.local_paths().unwrap()[0]),
            selection.paths[0]
        );
        assert!(Selection {
            paths: vec![b"relative".to_vec()],
            ..Default::default()
        }
        .local_paths()
        .is_err());
        config.filters[0].rules[0].0 = 2;
        assert!(config.validate().is_err());
    }
}
