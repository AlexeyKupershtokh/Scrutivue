//! Explicit per-user Open with registration. Never change default applications.
use anyhow::{ensure, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::{
    ffi::{c_void, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr,
};

const OWNER: &str = "Scrutivue.OpenWith.v1";
const PROG_ID: &str = "Scrutivue.Image";
const APPLICATION: &str = "Applications\\scrutivue.exe";
const OWNER_VALUE: &str = "ScrutivueOwner";
const EXE_VALUE: &str = "ScrutivueExecutable";

#[derive(Clone, Copy)]
pub enum Action {
    Status,
    Register,
    Remove,
}

pub struct Worker {
    jobs: Sender<Action>,
    pub events: Receiver<Result<String, String>>,
}
impl Worker {
    pub fn new() -> Self {
        let (jobs, actions) = bounded(2);
        let (sender, events) = bounded(4);
        std::thread::Builder::new().name("windows-open-with".into()).spawn(move || {
            for action in actions {
                let result = (|| -> Result<String> {
                    let exe = std::env::current_exe().context("Find the running application")?;
                    let registration = Registration::new(
                        "Software\\Classes".into(),
                        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts".into(),
                        &exe,
                    )?;
                    let message = match action {
                        Action::Status => registration.status()?,
                        Action::Register => {
                            registration.register()?;
                            notify_shell();
                            "Registered for Open with for this Windows user. Default apps were not changed.".into()
                        }
                        Action::Remove => {
                            registration.remove()?;
                            notify_shell();
                            "Open with registration removed. Other viewers and default apps were preserved.".into()
                        }
                    };
                    Ok(message)
                })().map_err(|error| format!("Windows Open with: {error:#}"));
                if sender.send(result).is_err() { break; }
            }
        }).expect("Windows integration worker");
        let worker = Self { jobs, events };
        let _ = worker.request(Action::Status);
        worker
    }
    pub fn request(&self, action: Action) -> Result<(), String> {
        self.jobs
            .try_send(action)
            .map_err(|_| "Windows integration is busy or unavailable. Try again.".into())
    }
}

struct Registration {
    classes: String,
    user_choices: String,
    exe: Vec<u16>,
    command: Vec<u16>,
}
impl Registration {
    fn new(classes: String, user_choices: String, exe: &Path) -> Result<Self> {
        ensure!(exe.is_absolute(), "The application path must be absolute");
        let exe = crate::file_actions::explorer_path(exe.as_os_str().encode_wide().collect());
        ensure!(
            !exe.contains(&0) && !exe.contains(&(b'"' as u16)),
            "Invalid application path"
        );
        let command = [
            vec![b'"' as u16],
            exe.clone(),
            "\" -- \"%1\"".encode_utf16().collect(),
        ]
        .concat();
        Ok(Self {
            classes,
            user_choices,
            exe,
            command,
        })
    }
    fn path(&self, suffix: &str) -> String {
        format!("{}\\{suffix}", self.classes)
    }
    fn roots(&self) -> [String; 2] {
        [self.path(PROG_ID), self.path(APPLICATION)]
    }
    fn status(&self) -> Result<String> {
        let roots = self.roots();
        let mut found = 0;
        for path in &roots {
            if let Some(key) = Key::open(path)? {
                if key.string(OWNER_VALUE)?.as_deref() != Some(&wide(OWNER)) {
                    return Ok("An existing Open with entry is not managed by Scrutivue and will be left unchanged.".into());
                }
                if let Some(exe) = key.string(EXE_VALUE)? {
                    if exe != self.exe {
                        return Ok(format!("Registered to another copy: {}. Register to use this executable instead.", PathBuf::from(OsString::from_wide(&exe)).display()));
                    }
                }
                found += 1;
            }
        }
        if found == 0 {
            return Ok("Not registered for Open with. Default apps are unchanged.".into());
        }
        let mut complete = found == 2;
        for path in &roots {
            complete &= Key::open(&format!("{path}\\shell\\open\\command"))?
                .map(|key| key.string(""))
                .transpose()?
                .flatten()
                .as_deref()
                == Some(self.command.as_slice());
        }
        for extension in crate::model::SUPPORTED_EXTENSIONS {
            complete &= Key::open(&self.path(&format!(".{extension}\\OpenWithProgids")))?
                .map(|key| key.value(PROG_ID))
                .transpose()?
                .flatten()
                == Some(Value {
                    kind: 0,
                    bytes: Vec::new(),
                });
        }
        Ok(if complete {
            "Registered for Open with using this executable. Default apps are unchanged."
        } else {
            "Open with registration is incomplete. Register to repair it."
        }
        .into())
    }
    fn register(&self) -> Result<()> {
        // Check both namespaces before writing anything; never adopt foreign keys.
        for path in self.roots() {
            if let Some(key) = Key::open(&path)? {
                ensure!(key.empty()? || key.string(OWNER_VALUE)?.as_deref() == Some(&wide(OWNER)),
                    "An existing registration is not managed by Scrutivue; it was left unchanged ({path})");
            }
        }
        for path in self.roots() {
            let key = Key::create(&path)?;
            key.set_string(OWNER_VALUE, &wide(OWNER))?;
            key.set_string(EXE_VALUE, &self.exe)?;
            key.set_string("FriendlyAppName", &wide("Scrutivue"))?;
            if path == self.path(PROG_ID) {
                key.set_string("", &wide("Scrutivue Image"))?;
            }
            let icon = [
                vec![b'"' as u16],
                self.exe.clone(),
                "\",0".encode_utf16().collect(),
            ]
            .concat();
            Key::create(&format!("{path}\\DefaultIcon"))?.set_string("", &icon)?;
            Key::create(&format!("{path}\\shell\\open\\command"))?.set_string("", &self.command)?;
        }
        let application = Key::create(&self.path(&format!("{PROG_ID}\\Application")))?;
        application.set_string("ApplicationName", &wide("Scrutivue"))?;
        application.set_string(
            "ApplicationDescription",
            &wide("Photograph and RAW comparison viewer"),
        )?;
        let types = Key::create(&self.path(&format!("{APPLICATION}\\SupportedTypes")))?;
        for extension in crate::model::SUPPORTED_EXTENSIONS {
            let extension = format!(".{extension}");
            types.set_string(&extension, &[])?;
            // Only add our named candidate. Never write the extension's default
            // value, UserChoice, OpenWithList MRU order, or machine-wide keys.
            Key::create(&self.path(&format!("{extension}\\OpenWithProgids")))?.set(
                PROG_ID,
                &Value {
                    kind: 0,
                    bytes: Vec::new(),
                },
            )?;
        }
        Ok(())
    }
    fn remove(&self) -> Result<()> {
        let mut owned = Vec::new();
        for path in self.roots() {
            if let Some(key) = Key::open(&path)? {
                ensure!(
                    key.string(OWNER_VALUE)?.as_deref() == Some(&wide(OWNER)),
                    "An existing registration is not managed by Scrutivue; it was left unchanged"
                );
                ensure!(key.string(EXE_VALUE)?.as_deref() == Some(self.exe.as_slice()), "Open with is registered to another copy. Remove it from that copy, or Register this copy first");
                if let Some(command) = Key::open(&format!("{path}\\shell\\open\\command"))?
                    .map(|key| key.string(""))
                    .transpose()?
                    .flatten()
                {
                    ensure!(
                        command == self.command,
                        "The registered open command was changed; it was left unchanged"
                    );
                }
                owned.push(path);
            }
        }
        if owned.is_empty() {
            return Ok(());
        }
        // If the user later chose Scrutivue as a default in Windows, deleting
        // its handler would break that choice even without writing UserChoice.
        for extension in crate::model::SUPPORTED_EXTENSIONS {
            for (path, value) in [
                (self.path(&format!(".{extension}")), ""),
                (
                    format!("{}\\.{extension}\\UserChoice", self.user_choices),
                    "ProgId",
                ),
            ] {
                if let Some(default) = Key::open(&path)?
                    .map(|key| key.string(value))
                    .transpose()?
                    .flatten()
                {
                    let default = String::from_utf16_lossy(&default);
                    ensure!(
                        ![PROG_ID, APPLICATION].iter().any(|id| default.eq_ignore_ascii_case(id)),
                        "Scrutivue is a default app for .{extension}. Choose another default in Windows Settings before removing its registration"
                    );
                }
            }
        }
        for extension in crate::model::SUPPORTED_EXTENSIONS {
            if let Some(key) = Key::open(&self.path(&format!(".{extension}\\OpenWithProgids")))? {
                if key.value(PROG_ID)?
                    == Some(Value {
                        kind: 0,
                        bytes: Vec::new(),
                    })
                {
                    // Reopen with write access only for this one named value.
                    Key::create(&self.path(&format!(".{extension}\\OpenWithProgids")))?
                        .delete_value(PROG_ID)?;
                }
            }
        }
        for path in owned {
            delete_tree(&path)?;
        }
        Ok(())
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}
fn terminated(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
type Handle = *mut c_void;
const CURRENT_USER: Handle = 0x8000_0001u32 as i32 as isize as Handle;
const KEY_READ: u32 = 0x20019;
const KEY_WRITE: u32 = 0x20006;
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: Handle,
        name: *const u16,
        options: u32,
        access: u32,
        result: *mut Handle,
    ) -> i32;
    fn RegCreateKeyExW(
        key: Handle,
        name: *const u16,
        reserved: u32,
        class: *mut u16,
        options: u32,
        access: u32,
        security: *const c_void,
        result: *mut Handle,
        disposition: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
    fn RegSetValueExW(
        key: Handle,
        name: *const u16,
        reserved: u32,
        kind: u32,
        data: *const u8,
        bytes: u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: Handle,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        bytes: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(key: Handle, name: *const u16) -> i32;
    fn RegDeleteTreeW(key: Handle, name: *const u16) -> i32;
    fn RegQueryInfoKeyW(
        key: Handle,
        class: *mut u16,
        class_len: *mut u32,
        reserved: *mut u32,
        subkeys: *mut u32,
        max_subkey: *mut u32,
        max_class: *mut u32,
        values: *mut u32,
        max_name: *mut u32,
        max_value: *mut u32,
        security: *mut u32,
        time: *mut c_void,
    ) -> i32;
}
fn check(status: i32, operation: &str) -> Result<()> {
    ensure!(status == 0, "{operation} failed (Windows error {status})");
    Ok(())
}
#[derive(Debug, PartialEq, Eq)]
struct Value {
    kind: u32,
    bytes: Vec<u8>,
}
struct Key(Handle);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
impl Key {
    fn open(path: &str) -> Result<Option<Self>> {
        let mut handle = ptr::null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                CURRENT_USER,
                terminated(path).as_ptr(),
                0,
                KEY_READ,
                &mut handle,
            )
        };
        if status == 2 || status == 3 {
            return Ok(None);
        }
        check(status, "Read Open with registration")?;
        Ok(Some(Self(handle)))
    }
    fn create(path: &str) -> Result<Self> {
        let mut handle = ptr::null_mut();
        check(
            unsafe {
                RegCreateKeyExW(
                    CURRENT_USER,
                    terminated(path).as_ptr(),
                    0,
                    ptr::null_mut(),
                    0,
                    KEY_READ | KEY_WRITE,
                    ptr::null(),
                    &mut handle,
                    ptr::null_mut(),
                )
            },
            "Write Open with registration",
        )?;
        Ok(Self(handle))
    }
    fn value(&self, name: &str) -> Result<Option<Value>> {
        let name = terminated(name);
        let mut bytes = 0;
        let mut kind = 0;
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                ptr::null_mut(),
                &mut kind,
                ptr::null_mut(),
                &mut bytes,
            )
        };
        if status == 2 {
            return Ok(None);
        }
        check(status, "Read Open with value")?;
        ensure!(
            bytes <= 65536,
            "Open with registry value exceeds size limit"
        );
        let mut data = vec![0; bytes as usize];
        check(
            unsafe {
                RegQueryValueExW(
                    self.0,
                    name.as_ptr(),
                    ptr::null_mut(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut bytes,
                )
            },
            "Read Open with value",
        )?;
        data.truncate(bytes as usize);
        Ok(Some(Value { kind, bytes: data }))
    }
    fn string(&self, name: &str) -> Result<Option<Vec<u16>>> {
        let Some(value) = self.value(name)? else {
            return Ok(None);
        };
        ensure!(
            value.kind == 1 && value.bytes.len() % 2 == 0,
            "Unexpected Open with registry value type"
        );
        let mut text: Vec<_> = value
            .bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        if text.last() == Some(&0) {
            text.pop();
        }
        Ok(Some(text))
    }
    fn set(&self, name: &str, value: &Value) -> Result<()> {
        check(
            unsafe {
                RegSetValueExW(
                    self.0,
                    terminated(name).as_ptr(),
                    0,
                    value.kind,
                    value.bytes.as_ptr(),
                    value.bytes.len() as u32,
                )
            },
            "Write Open with value",
        )
    }
    fn set_string(&self, name: &str, text: &[u16]) -> Result<()> {
        let bytes = text
            .iter()
            .copied()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        self.set(name, &Value { kind: 1, bytes })
    }
    fn delete_value(&self, name: &str) -> Result<()> {
        let status = unsafe { RegDeleteValueW(self.0, terminated(name).as_ptr()) };
        if status == 2 {
            return Ok(());
        }
        check(status, "Remove Open with candidate")
    }
    fn empty(&self) -> Result<bool> {
        let (mut subkeys, mut values) = (0, 0);
        check(
            unsafe {
                RegQueryInfoKeyW(
                    self.0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut subkeys,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut values,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            "Inspect Open with registration",
        )?;
        Ok(subkeys == 0 && values == 0)
    }
}
fn delete_tree(path: &str) -> Result<()> {
    let status = unsafe { RegDeleteTreeW(CURRENT_USER, terminated(path).as_ptr()) };
    if status == 2 || status == 3 {
        return Ok(());
    }
    check(status, "Remove owned Open with registration")
}
fn notify_shell() {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHChangeNotify(event: i32, flags: u32, item1: *const c_void, item2: *const c_void);
    }
    unsafe {
        SHChangeNotify(0x0800_0000, 0, ptr::null(), ptr::null());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = format!(
                "Software\\Scrutivue\\Tests\\OpenWith\\{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            assert!(Key::open(&root).unwrap().is_none());
            Key::create(&root).unwrap();
            Self { root }
        }
        fn registration(&self, name: &str) -> Registration {
            Registration::new(
                format!("{}\\Classes", self.root),
                format!("{}\\Explorer\\FileExts", self.root),
                Path::new(name),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            delete_tree(&self.root).unwrap();
        }
    }
    #[test]
    fn registration_covers_exactly_the_supported_extensions() {
        let mut unique = std::collections::HashSet::new();
        for extension in crate::model::SUPPORTED_EXTENSIONS {
            assert!(unique.insert(extension));
            assert!(extension
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
            assert!(crate::model::supported(Path::new(&format!(
                "image.{}",
                extension.to_uppercase()
            ))));
        }
        assert_eq!(unique.len(), 27);
    }
    #[test]
    fn open_command_quotes_unicode_spaces_and_shell_characters_without_a_shell() {
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn CommandLineToArgvW(command: *const u16, count: *mut i32) -> *mut *mut u16;
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LocalFree(memory: *mut c_void) -> *mut c_void;
        }
        for path in [
            r"C:\Portable photos\Scrutivue — 日本語 & 100%.exe",
            r"\\?\C:\Portable photos\scrutivue.exe",
            r"\\?\UNC\server\share\scrutivue.exe",
        ] {
            let registration =
                Registration::new("test".into(), "test-choices".into(), Path::new(path)).unwrap();
            let file = r"C:\Photos\--gain & 日本語 %.NEF";
            let command = [
                vec![b'"' as u16],
                registration.exe.clone(),
                "\" -- \"".encode_utf16().collect(),
                wide(file),
                vec![b'"' as u16, 0],
            ]
            .concat();
            let mut count = 0;
            let argv = unsafe { CommandLineToArgvW(command.as_ptr(), &mut count) };
            assert!(!argv.is_null());
            assert_eq!(count, 3);
            let values: Vec<Vec<u16>> = (0..count as usize)
                .map(|index| unsafe {
                    let value = *argv.add(index);
                    let mut size = 0;
                    while *value.add(size) != 0 {
                        size += 1;
                    }
                    std::slice::from_raw_parts(value, size).to_vec()
                })
                .collect();
            unsafe {
                LocalFree(argv.cast());
            }
            assert_eq!(values, [registration.exe, wide("--"), wide(file)]);
        }
        assert!(Registration::new(
            "test".into(),
            "test-choices".into(),
            Path::new("relative.exe")
        )
        .is_err());
        assert!(Registration::new(
            "test".into(),
            "test-choices".into(),
            Path::new("C:\\bad\"name.exe")
        )
        .is_err());
    }
    #[test]
    fn register_and_remove_are_idempotent_and_preserve_defaults_and_other_viewers() {
        let fixture = Fixture::new();
        let registration = fixture.registration(r"C:\Portable app\scrutivue.exe");
        let extension = Key::create(&registration.path(".nef")).unwrap();
        extension.set_string("", &wide("Other.Image")).unwrap();
        let others = Key::create(&registration.path(".nef\\OpenWithProgids")).unwrap();
        others
            .set(
                "Other.Image",
                &Value {
                    kind: 0,
                    bytes: vec![],
                },
            )
            .unwrap();
        let choice = Key::create(&format!(
            "{}\\Explorer\\FileExts\\.nef\\UserChoice",
            fixture.root
        ))
        .unwrap();
        choice.set_string("ProgId", &wide("Other.Image")).unwrap();
        assert!(registration.status().unwrap().starts_with("Not registered"));
        registration.register().unwrap();
        registration.register().unwrap();
        assert!(registration.status().unwrap().starts_with("Registered for"));
        for extension in crate::model::SUPPORTED_EXTENSIONS {
            assert_eq!(
                Key::open(&registration.path(&format!(".{extension}\\OpenWithProgids")))
                    .unwrap()
                    .unwrap()
                    .value(PROG_ID)
                    .unwrap(),
                Some(Value {
                    kind: 0,
                    bytes: vec![]
                })
            );
        }
        assert_eq!(
            Key::open(&registration.path(&format!("{APPLICATION}\\shell\\open\\command")))
                .unwrap()
                .unwrap()
                .string("")
                .unwrap(),
            Some(registration.command.clone())
        );
        registration.remove().unwrap();
        registration.remove().unwrap();
        assert_eq!(extension.string("").unwrap(), Some(wide("Other.Image")));
        assert_eq!(choice.string("ProgId").unwrap(), Some(wide("Other.Image")));
        assert!(others.value("Other.Image").unwrap().is_some());
        assert!(others.value(PROG_ID).unwrap().is_none());
        assert!(Key::open(&registration.path(APPLICATION))
            .unwrap()
            .is_none());
        assert!(Key::open(&registration.path(PROG_ID)).unwrap().is_none());
    }
    #[test]
    fn unowned_existing_application_is_never_overwritten_or_removed() {
        let fixture = Fixture::new();
        let registration = fixture.registration(r"C:\Portable app\scrutivue.exe");
        let foreign = Key::create(&registration.path(APPLICATION)).unwrap();
        foreign
            .set_string("FriendlyAppName", &wide("Keep this registration"))
            .unwrap();
        assert!(registration.register().is_err());
        assert!(registration.remove().is_err());
        assert_eq!(
            foreign.string("FriendlyAppName").unwrap(),
            Some(wide("Keep this registration"))
        );
        assert!(Key::open(&registration.path(PROG_ID)).unwrap().is_none());
    }
    #[test]
    fn registration_can_move_but_an_older_copy_cannot_remove_the_newer_copy() {
        let fixture = Fixture::new();
        let old = fixture.registration(r"C:\Old app\scrutivue.exe");
        let new = fixture.registration(r"D:\New app\scrutivue.exe");
        old.register().unwrap();
        new.register().unwrap();
        assert!(old.remove().is_err());
        assert!(new.status().unwrap().starts_with("Registered for"));
        new.remove().unwrap();
    }
    #[test]
    fn a_changed_open_command_is_not_deleted() {
        let fixture = Fixture::new();
        let registration = fixture.registration(r"C:\Portable app\scrutivue.exe");
        registration.register().unwrap();
        let command =
            Key::create(&registration.path(&format!("{APPLICATION}\\shell\\open\\command")))
                .unwrap();
        command
            .set_string("", &wide("a manually edited command"))
            .unwrap();
        assert!(registration.remove().is_err());
        assert_eq!(
            command.string("").unwrap(),
            Some(wide("a manually edited command"))
        );
        assert!(Key::open(&registration.path(PROG_ID)).unwrap().is_some());
    }
    #[test]
    fn removal_preserves_a_handler_that_the_user_later_chose_as_default() {
        let fixture = Fixture::new();
        let registration = fixture.registration(r"C:\Portable app\scrutivue.exe");
        registration.register().unwrap();
        for (path, value, id) in [
            (registration.path(".nef"), "", "scrutivue.image"),
            (
                format!("{}\\.nef\\UserChoice", registration.user_choices),
                "ProgId",
                APPLICATION,
            ),
        ] {
            let key = Key::create(&path).unwrap();
            key.set_string(value, &wide(id)).unwrap();
            assert!(registration
                .remove()
                .unwrap_err()
                .to_string()
                .contains("Choose another default"));
            assert!(Key::open(&registration.path(PROG_ID)).unwrap().is_some());
            assert_eq!(key.string(value).unwrap(), Some(wide(id)));
            key.delete_value(value).unwrap();
        }
        registration.remove().unwrap();
    }
}
