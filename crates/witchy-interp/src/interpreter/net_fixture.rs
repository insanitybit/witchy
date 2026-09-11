//! Test-only driver for the EXISTING source Net builtin paths. No dial hook,
//! ambient grants, new receive implementation, or production entrypoint.
use super::*;

#[derive(Debug, Clone)]
pub enum NetFixtureCall { Bytes(i64), Line, All, Close, Send(String) }

pub struct NetReceiveFixture { interpreter: Interpreter }

impl NetReceiveFixture {
    pub fn new(stream: Box<dyn Stream>) -> Self {
        let mut interpreter = Interpreter::new_with_witnesses(
            parse_module("").expect("empty fixture module"), WitnessPlan::default());
        interpreter.sockets.push(BufReader::new(stream));
        Self { interpreter }
    }

    pub fn buffer_capacity(&self) -> usize { self.interpreter.sockets[0].capacity() }

    pub fn call(&mut self, call: NetFixtureCall) -> Result<Option<String>, RuntimeError> {
        let mut args = vec![Value::Socket(0)];
        let name = match call {
            NetFixtureCall::Bytes(count) => { args.push(Value::Int(count)); "recv_bytes" }
            NetFixtureCall::Line => "recv_line",
            NetFixtureCall::All => "recv_all",
            NetFixtureCall::Close => "close",
            NetFixtureCall::Send(text) => { args.push(Value::str(text)); "send_bytes" }
        };
        match self.interpreter.call_builtin(name, &args)? {
            Some(Value::Str(text)) => Ok(Some(text.to_string())),
            Some(Value::Unit) => Ok(None),
            _ => Err(RuntimeError { message: "unexpected Net fixture builtin result".into() }),
        }
    }
}
