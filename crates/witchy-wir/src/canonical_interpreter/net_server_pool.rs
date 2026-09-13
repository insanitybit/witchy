//! A bounded schedule of the EXISTING server pool: all workers rendezvous,
//! then each finite worker runs to completion before the primary continues.
//! Pre-rendezvous execution may only compute in its private heap and obtain
//! the shared listener. Other host effects refuse, so moving those pure
//! prefixes ahead of the sequential suffixes preserves the startup barrier.
//! Scripts assign disjoint connections to workers; they do not supply guest
//! results. Cross-worker socket traffic and unbounded servers are not modeled.
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleServerPoolFixture {
    /// One wire script per additional worker (the primary has its Net script).
    /// Set this to the native launch's additional-worker count for parity.
    pub workers: Vec<Vec<OracleNetEvent>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OracleServerWorkerRun {
    pub output: Vec<String>,
    pub net_trace: Vec<OracleNetEvent>,
    pub leaked_objects: usize,
}

pub(in super::super) struct ServerLaunch {
    function: FunctionId,
    capabilities: Vec<InterpreterValue>,
    arguments: Vec<String>,
}

pub(in super::super) struct ServerState {
    fixture: Option<OracleServerPoolFixture>,
    launch: Option<ServerLaunch>,
    worker: bool,
    rendezvous: bool,
    pub(in super::super) completed: Vec<OracleServerWorkerRun>,
}

impl ServerState {
    pub(in super::super) fn new(fixture: Option<OracleServerPoolFixture>) -> Self {
        Self { fixture, launch: None, worker: false, rendezvous: false, completed: Vec::new() }
    }

    pub(in super::super) fn retain_launch(&mut self, function: FunctionId,
        capabilities: &[InterpreterValue], arguments: &[String]) -> Result<(), InterpretError>
    {
        if self.fixture.is_none() { return Ok(()); }
        // A fresh launch request is not a parent heap object, captured owner,
        // narrowed resource or another instance's host token. Restrict the
        // profile to Net and Console: other shared host worlds need their own
        // effect ordering, not copies of an oracle's mutable fixture state.
        for value in capabilities {
            let InterpreterValue::Capability(token) = value else {
                return Err(refused(OracleNetError::Unauthenticated));
            };
            if !matches!(token.kind, 0 | NET) || *token != CapabilityToken::root(token.kind) {
                return Err(refused(OracleNetError::Unauthenticated));
            }
        }
        self.launch = Some(ServerLaunch { function, capabilities: capabilities.to_vec(), arguments: arguments.to_vec() });
        Ok(())
    }

    pub(in super::super) fn before_host_call(&self, name: &str) -> Result<(), InterpretError> {
        if self.worker && !self.rendezvous && !matches!(name,
            "net_listen_at" | "serve_pool" | "__witchy_abort_at" | "__witchy_list_bounds_at")
        {
            return Err(InterpretError::UnsupportedOperation("server worker startup effect ordering"));
        }
        Ok(())
    }

    pub(in super::super) fn finish(&self) -> Result<(), InterpretError> {
        if let Some(fixture) = &self.fixture {
            if self.launch.is_none() || !self.rendezvous || self.completed.len() != fixture.workers.len() {
                return Err(refused(OracleNetError::IncompleteTranscript));
            }
        }
        Ok(())
    }
}

impl<'a> Engine<'a> {
    pub(in super::super) fn serve_pool(&mut self, token: &CapabilityToken) -> Result<(), InterpretError> {
        let net = self.net.as_ref().ok_or(InterpretError::UnsupportedHostImport)?;
        let Resource::Listener(listener) = net.authenticate(token, LISTENER)? else {
            return Err(refused(OracleNetError::WrongKind));
        };
        if self.server.worker {
            if net.shared_listener.as_ref().map(|(id, _)| *id) != Some(listener) {
                return Err(refused(OracleNetError::Unauthenticated));
            }
            self.server.rendezvous = true;
            return Ok(());
        }
        let launch = self.server.launch.as_ref().ok_or(InterpretError::UnsupportedOperation("server pool requires retained entry launch"))?;
        let fixture = self.server.fixture.as_ref().ok_or(InterpretError::UnsupportedOperation("server pool requires worker scripts"))?;
        if self.server.rendezvous { return Err(InterpretError::UnsupportedOperation("repeated primary server pool")); }
        let address = net.listener_addresses.get(&listener).ok_or(refused(OracleNetError::Unauthenticated))?.clone();
        // No DNS/alias assumptions: this profile requires one numeric address.
        address.parse::<std::net::SocketAddr>().map_err(|_| InterpretError::UnsupportedOperation("server pool numeric endpoint"))?;
        let mut ids: BTreeSet<_> = net.sockets.keys().copied().collect();
        for event in net.events.iter().chain(fixture.workers.iter().flatten()) {
            if let OracleNetEvent::Connect { socket, .. } | OracleNetEvent::Accept { socket, .. } = event
                && !ids.insert(*socket)
            {
                return Err(refused(OracleNetError::DuplicateResource));
            }
        }
        let function = launch.function;
        let capabilities = launch.capabilities.clone();
        let arguments = launch.arguments.clone();
        let scripts = fixture.workers.clone();
        let grants = net.grants.clone();
        let signature = self.function(function)?.signature;
        let parameters = self.module.signatures().iter().find(|sig| sig.id == signature)
            .ok_or(InterpretError::TypeMismatch("server entry signature"))?.parameters.clone();
        self.server.rendezvous = true;
        for transcript in scripts {
            let mut config = self.config.clone();
            config.max_steps = self.config.max_steps.checked_sub(self.steps).ok_or(InterpretError::StepLimitExceeded)?;
            config.console_trace = None;
            config.fixture = OracleFixture { net: Some(OracleNetFixture { grants: grants.clone(), transcript }),
                dir_grants: Some(0), ..Default::default() };
            let mut worker = Engine::new(self.module, config);
            worker.server.worker = true;
            worker.net.as_mut().unwrap().shared_listener = Some((listener, address.clone()));
            let values = worker.prepare_launch(&parameters, capabilities.clone(), &arguments)?;
            let execution = worker.execute(function, values);
            self.steps = self.steps.checked_add(worker.steps).ok_or(InterpretError::StepLimitExceeded)?;
            execution?;
            if !worker.server.rendezvous {
                return Err(InterpretError::UnsupportedOperation("server worker returned before startup rendezvous"));
            }
            let net_trace = worker.finish_net()?;
            let leaked_objects = worker.objects.values().filter(|object|
                matches!(object.storage, RuntimeStorage::Allocation(_))).count()
                + worker.owners.len() + worker.owner_lists.len();
            if leaked_objects != 0 || !worker.services.workers_empty() {
                return Err(InterpretError::UnsupportedOperation("server worker leaked owned state"));
            }
            self.server.completed.push(OracleServerWorkerRun { output: worker.output, net_trace, leaked_objects });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_pool_completion_requires_rendezvous_and_every_worker() {
        let mut server = ServerState::new(Some(OracleServerPoolFixture { workers: vec![vec![]] }));
        server.retain_launch(FunctionId::new(0), &[], &[]).unwrap();
        assert_eq!(server.finish(), Err(refused(OracleNetError::IncompleteTranscript)));
        server.rendezvous = true;
        assert_eq!(server.finish(), Err(refused(OracleNetError::IncompleteTranscript)));
        server.completed.push(OracleServerWorkerRun { output: vec![], net_trace: vec![], leaked_objects: 0 });
        server.finish().unwrap();
        let empty = ServerState::new(Some(OracleServerPoolFixture::default()));
        assert_eq!(empty.finish(), Err(refused(OracleNetError::IncompleteTranscript)),
            "even a zero-additional-worker pool must reach its configured rendezvous");
    }

    #[test]
    fn server_pool_launch_rejects_parent_objects_foreign_tokens_and_other_grants() {
        let fixture = Some(OracleServerPoolFixture::default());
        let net = NetState::new(OracleNetFixture { grants: vec![OracleNetGrant::default()], transcript: vec![] });
        for argument in [InterpreterValue::ObjectRef(0), InterpreterValue::capability(DIR_KIND),
            InterpreterValue::Capability(net.token(Resource::Grant(0)))]
        {
            let mut server = ServerState::new(fixture.clone());
            assert!(server.retain_launch(FunctionId::new(0), &[argument], &[]).is_err());
            assert!(server.launch.is_none());
        }
        let mut server = ServerState::new(fixture);
        server.retain_launch(FunctionId::new(9), &[InterpreterValue::capability(NET)], &["input".into()]).unwrap();
        assert_eq!(server.launch.as_ref().unwrap().function, FunctionId::new(9));
        assert_eq!(server.launch.as_ref().unwrap().arguments, ["input"]);
        server.worker = true;
        for effect in ["console_print", "net_connect_at", "net_accept", "exec_run_slices"] {
            assert!(server.before_host_call(effect).is_err(), "startup effect: {effect}");
        }
        server.before_host_call("net_listen_at").unwrap();
        server.before_host_call("serve_pool").unwrap();
    }

    #[test]
    fn server_pool_shared_listener_reissues_identity_without_widening_rights() {
        let address = "127.0.0.1:12345";
        let fixture = OracleNetFixture { grants: vec![OracleNetGrant { listen: true, connect: false,
            addresses: [address.to_owned()].into_iter().collect() }],
            transcript: vec![OracleNetEvent::Listen { grant: 0, address: address.into(), listener: 3 }] };
        let mut parent = NetState::new(fixture.clone());
        let parent_listener = parent.open(&parent.token(Resource::Grant(0)), address, true).unwrap();
        let mut child = NetState::new(OracleNetFixture { transcript: vec![], ..fixture });
        child.shared_listener = Some((3, address.into()));
        assert!(child.authenticate(&parent_listener, LISTENER).is_err());
        assert!(child.open(&parent.token(Resource::Grant(0)), address, true).is_err());
        assert!(child.authenticate(&CapabilityToken::root(LISTENER), LISTENER).is_err());
        let child_root = child.token(Resource::Grant(0));
        child.grants[0].listen = false;
        assert_eq!(child.open(&child_root, address, true), Err(refused(OracleNetError::RightsDenied)));
        child.grants[0].listen = true;
        child.grants[0].addresses.clear();
        assert_eq!(child.open(&child_root, address, true), Err(refused(OracleNetError::AddressDenied)));
        child.grants[0].addresses.insert(address.into());
        let child_listener = child.open(&child_root, address, true).unwrap();
        assert_ne!(child_listener, parent_listener);
        assert!(parent.authenticate(&child_listener, LISTENER).is_err());
        assert_eq!(child.authenticate(&child_listener, LISTENER).unwrap(), Resource::Listener(3));
        assert!(child.finish().unwrap().is_empty(), "sharing does not bind a new OS listener");
    }
}
