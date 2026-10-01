//! Pinned target scope, prepared edits and verified text execution.
use super::path_policy::{denied, reject_control_directories, validate_relative};
use super::platform::{
    open_pinned_file, path_within, pin_directories, verify_file, verify_handle_path,
};
use super::{ActionError, ActionReceipt, MAX_FILE_BYTES, TextEdit};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ScopedFiles {
    root: PathBuf,
    allowed: BTreeSet<String>,
    protected: Vec<PathBuf>,
    _root_handles: Vec<File>,
}

impl ScopedFiles {
    /// `protected` includes active policy/configuration, state, source and
    /// installation paths. The caller is trusted assembly, never model input.
    pub fn open(
        root: &Path,
        allowed: &[String],
        protected: &[PathBuf],
    ) -> Result<Self, ActionError> {
        if !root.is_absolute() || allowed.is_empty() || allowed.len() > 64 {
            return Err(denied("absolute target and 1..64 allowed files required"));
        }
        reject_control_directories(root)?;
        let handles = pin_directories(root)?;
        let root = root.canonicalize()?;
        reject_control_directories(&root)?;
        let root_handle = handles
            .last()
            .ok_or_else(|| denied("target root handle is unavailable"))?;
        verify_handle_path(root_handle, &root)?;
        let mut protected_paths = Vec::new();
        for path in protected {
            let canonical = path.canonicalize()?;
            if path_within(&root, &canonical)? {
                return Err(denied("target lies inside a protected path"));
            }
            protected_paths.push(canonical);
        }
        let mut names = BTreeSet::new();
        for name in allowed {
            validate_relative(name)?;
            if !names.insert(name.clone()) {
                return Err(denied("duplicate allowed file"));
            }
        }
        let scoped = Self {
            root,
            allowed: names,
            protected: protected_paths,
            _root_handles: handles,
        };
        for name in &scoped.allowed {
            // Detect missing, linked, protected or oversized targets at startup.
            scoped.read(name)?;
        }
        Ok(scoped)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn allowed_files(&self) -> Vec<String> {
        self.allowed.iter().cloned().collect()
    }

    pub fn read(&self, path: &str) -> Result<String, ActionError> {
        let (mut file, _handles) = self.open_file(path, false)?;
        read_bounded(&mut file).map_err(Into::into)
    }

    pub(crate) fn prepare(&self, edit: &TextEdit) -> Result<PreparedEdit, ActionError> {
        if edit.expected.len() > MAX_FILE_BYTES || edit.replacement.len() > MAX_FILE_BYTES {
            return Err(denied("text exceeds 16 KiB"));
        }
        let (mut file, handles) = self.open_file(&edit.path, true)?;
        if read_bounded(&mut file)? != edit.expected {
            return Err(ActionError::Changed);
        }
        Ok(PreparedEdit {
            file,
            _handles: handles,
            expected_path: self.root.join(&edit.path),
            edit: edit.clone(),
        })
    }

    fn open_file(&self, name: &str, write: bool) -> Result<(File, Vec<File>), ActionError> {
        validate_relative(name)?;
        if !self.allowed.contains(name) {
            return Err(denied("file not explicitly allowed"));
        }
        let path = self.root.join(name);
        let parent = path.parent().ok_or_else(|| denied("file has no parent"))?;
        let handles = pin_directories(parent)?;
        let file = open_pinned_file(&path, write)?;
        // Resolve the object we actually opened, never a second path lookup.
        // A directory can acquire/remove a reparse attribute despite a handle
        // denying delete sharing; that cannot redirect this identity check.
        let actual = verify_handle_path(&file, &path)?;
        if !path_within(&actual, &self.root)? {
            return Err(denied("file resolves to a protected or out-of-scope path"));
        }
        for protected in &self.protected {
            if path_within(&actual, protected)? {
                return Err(denied("file resolves to a protected or out-of-scope path"));
            }
        }
        verify_file(&file)?;
        Ok((file, handles))
    }
}

pub(crate) struct PreparedEdit {
    file: File,
    _handles: Vec<File>,
    expected_path: PathBuf,
    edit: TextEdit,
}

impl PreparedEdit {
    /// Only the trusted workflow invokes this, after consuming its exact
    /// persisted permit. Handles forbid concurrent write/delete until receipt.
    pub(crate) fn execute(mut self) -> Result<ActionReceipt, ActionError> {
        verify_handle_path(&self.file, &self.expected_path)?;
        verify_file(&self.file)?;
        if read_bounded(&mut self.file)? != self.edit.expected {
            return Err(ActionError::Changed);
        }
        self.file.seek(SeekFrom::Start(0))?;
        let write_result = (|| -> io::Result<()> {
            self.file.write_all(self.edit.replacement.as_bytes())?;
            self.file.set_len(self.edit.replacement.len() as u64)?;
            self.file.sync_all()?;
            if read_bounded(&mut self.file)? != self.edit.replacement {
                return Err(io::Error::other(
                    "read-back did not match approved replacement",
                ));
            }
            Ok(())
        })();
        write_result.map_err(|error| ActionError::Unknown(error.to_string()))?;
        Ok(ActionReceipt {
            path: self.edit.path,
            bytes_written: self.edit.replacement.len(),
            content_verified: true,
        })
    }
}

fn read_bounded(file: &mut File) -> io::Result<String> {
    if file.metadata()?.len() > MAX_FILE_BYTES as u64 {
        return Err(io::Error::other("file exceeds 16 KiB"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(io::Error::other("file exceeds 16 KiB"));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("file is not UTF-8"))
}
