//! Port of senpi `packages/agent/src/transport/in-process-carrier.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::local_endpoint::{LocalEndpoint, endpoint_key};
use super::transport::{CarrierConnection, CarrierMessage, CarrierSide};

pub type ScheduleFn = Arc<dyn Fn(Box<dyn FnOnce() + Send + 'static>) + Send + Sync>;
pub type LinkUnlisten = Box<dyn FnOnce() + Send>;

#[derive(Clone, Default)]
pub struct InProcessNetworkOptions {
    pub schedule: Option<ScheduleFn>,
}

fn default_schedule() -> ScheduleFn {
    Arc::new(|deliver| {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                deliver();
            });
        } else {
            deliver();
        }
    })
}

struct LinkState {
    generation: u64,
    up: bool,
}

#[derive(Clone)]
pub struct CarrierLink {
    a: Arc<dyn CarrierSide>,
    b: Arc<dyn CarrierSide>,
    schedule: ScheduleFn,
    state: Arc<Mutex<LinkState>>,
}

struct HalfConnection {
    to: Arc<dyn CarrierSide>,
    at: u64,
    schedule: ScheduleFn,
    state: Arc<Mutex<LinkState>>,
    link: CarrierLink,
}

impl CarrierConnection for HalfConnection {
    fn send(&self, message: CarrierMessage) -> bool {
        let to = Arc::clone(&self.to);
        let at = self.at;
        let state = Arc::clone(&self.state);
        (self.schedule)(Box::new(move || {
            let active = {
                let guard = state.lock().unwrap_or_else(|p| p.into_inner());
                guard.up && guard.generation == at
            };
            if active {
                to.message(&message);
            }
        }));
        true
    }

    fn close(&self) {
        let should_drop = {
            let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            guard.generation == self.at
        };
        if should_drop {
            self.link.drop_link();
        }
    }
}

impl CarrierLink {
    pub fn connect(&self) {
        let generation = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.up {
                return;
            }
            guard.up = true;
            guard.generation += 1;
            guard.generation
        };
        let half_to_b = Arc::new(HalfConnection {
            to: Arc::clone(&self.b),
            at: generation,
            schedule: Arc::clone(&self.schedule),
            state: Arc::clone(&self.state),
            link: self.clone(),
        });
        let half_to_a = Arc::new(HalfConnection {
            to: Arc::clone(&self.a),
            at: generation,
            schedule: Arc::clone(&self.schedule),
            state: Arc::clone(&self.state),
            link: self.clone(),
        });
        self.a.attached(half_to_b);
        self.b.attached(half_to_a);
    }

    pub fn drop_link(&self) {
        {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if !guard.up {
                return;
            }
            guard.up = false;
            guard.generation += 1;
        }
        self.a.detached();
        self.b.detached();
    }

    pub fn connected(&self) -> bool {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).up
    }
}

struct ListenerEntry {
    side: Arc<dyn CarrierSide>,
    dialed: bool,
}

#[derive(Clone)]
pub struct InProcessNetwork {
    schedule: ScheduleFn,
    listeners: Arc<Mutex<HashMap<String, ListenerEntry>>>,
}

impl InProcessNetwork {
    pub fn listen(
        &self,
        endpoint: &LocalEndpoint,
        side: Arc<dyn CarrierSide>,
    ) -> Result<LinkUnlisten, String> {
        let key = endpoint_key(endpoint);
        let mut listeners = self.listeners.lock().unwrap_or_else(|p| p.into_inner());
        if listeners.contains_key(&key) {
            return Err(format!(
                "in-process carrier: {} already has a listener",
                endpoint.address()
            ));
        }
        listeners.insert(
            key.clone(),
            ListenerEntry {
                side,
                dialed: false,
            },
        );
        let map = Arc::clone(&self.listeners);
        Ok(Box::new(move || {
            map.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
        }))
    }

    pub fn dial(
        &self,
        endpoint: &LocalEndpoint,
        side: Arc<dyn CarrierSide>,
    ) -> Result<CarrierLink, String> {
        let key = endpoint_key(endpoint);
        let mut listeners = self.listeners.lock().unwrap_or_else(|p| p.into_inner());
        let Some(listener) = listeners.get_mut(&key) else {
            return Err(format!(
                "in-process carrier: no listener at {}",
                endpoint.address()
            ));
        };
        if listener.dialed {
            return Err(format!(
                "in-process carrier: {} is already dialed",
                endpoint.address()
            ));
        }
        listener.dialed = true;
        Ok(CarrierLink {
            a: side,
            b: Arc::clone(&listener.side),
            schedule: Arc::clone(&self.schedule),
            state: Arc::new(Mutex::new(LinkState {
                generation: 0,
                up: false,
            })),
        })
    }
}

pub fn create_in_process_network(options: InProcessNetworkOptions) -> InProcessNetwork {
    InProcessNetwork {
        schedule: options.schedule.unwrap_or_else(default_schedule),
        listeners: Arc::new(Mutex::new(HashMap::new())),
    }
}
