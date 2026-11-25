use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

pub(super) struct CompilerSession {
    socket: PathBuf,
    child: Child,
}

impl CompilerSession {
    pub(super) fn start(root: &Path, configure: impl FnOnce(&mut Command)) -> Self {
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("socket");
        let mut command = Command::new(env!("CARGO_BIN_EXE_witchyc"));
        command
            .arg("compiler-session")
            .arg(&socket)
            .current_dir(root)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        configure(&mut command);
        let mut child = command.spawn().unwrap();
        let mut header = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut header)
            .unwrap();
        assert_eq!(header.trim_end(), "WITCHY_COMPILER_SESSION_1");
        Self { socket, child }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(super) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn request(&self, fields: &[&str]) -> (u32, [String; 4]) {
        let mut socket = UnixStream::connect(&self.socket).unwrap();
        socket.write_all(&(fields.len() as u32).to_be_bytes()).unwrap();
        for field in fields {
            socket.write_all(&(field.len() as u32).to_be_bytes()).unwrap();
            socket.write_all(field.as_bytes()).unwrap();
        }
        fn read_word(socket: &mut UnixStream) -> u32 {
            let mut bytes = [0; 4];
            socket.read_exact(&mut bytes).unwrap();
            u32::from_be_bytes(bytes)
        }
        let status = read_word(&mut socket);
        let fields = std::array::from_fn(|_| {
            let mut bytes = vec![0; read_word(&mut socket) as usize];
            socket.read_exact(&mut bytes).unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (status, fields)
    }

    pub(super) fn finish(mut self) {
        drop(self.child.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}
