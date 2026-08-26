//! Simulation harness primitives for ArkFS library tests.
//!
//! Deterministic clock, Earth/Moon/Mars delay model, bit-flip and node-loss chaos.
//!
//! This crate does **not** implement the object store or temporal index. It
//! records *intent* (advance time, inject a flip, mark a node lost) so tests
//! can apply those effects to a real `persistent_object_store` later.
//! The Elixir package of the same name is the scenario DSL / oracle; keep
//! the two models aligned (Earth/Moon/Mars, clock_rate, mars_delay_ms).

use arkfs_core::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Logical body for delay presets (one-way ms in [`default_delay_ms`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Body {
    Earth,
    Moon,
    Mars,
}

/// One-way delay in milliseconds between bodies (symmetric for simplicity in v1).
pub fn default_delay_ms(a: Body, b: Body) -> u64 {
    use Body::*;
    match (a, b) {
        (Earth, Earth) | (Moon, Moon) | (Mars, Mars) => 0,
        (Earth, Moon) | (Moon, Earth) => 1_300,
        (Earth, Mars) | (Mars, Earth) | (Moon, Mars) | (Mars, Moon) => 240_000,
    }
}

/// Deterministic virtual clock shared by a scenario (mutex so tests can clone it).
#[derive(Debug, Clone)]
pub struct VirtualClock {
    inner: Arc<Mutex<ClockState>>,
}

#[derive(Debug)]
struct ClockState {
    /// Virtual nanoseconds since scenario start.
    now_ns: u64,
    /// Speed multiplier (1.0 = realtime virtual; higher = faster).
    rate: f64,
    logical: u64,
}

impl VirtualClock {
    pub fn new() -> Self {
        VirtualClock {
            inner: Arc::new(Mutex::new(ClockState {
                now_ns: 0,
                rate: 1.0,
                logical: 0,
            })),
        }
    }

    pub fn set_rate(&self, rate: f64) {
        self.inner.lock().unwrap().rate = rate.max(0.0);
    }

    pub fn now_ns(&self) -> u64 {
        self.inner.lock().unwrap().now_ns
    }

    pub fn advance_ms(&self, ms: u64) {
        let mut g = self.inner.lock().unwrap();
        let delta = ((ms as f64) * 1_000_000.0 * g.rate) as u64;
        g.now_ns = g.now_ns.saturating_add(delta);
        g.logical = g.logical.saturating_add(1);
    }

    pub fn timestamp(&self) -> Timestamp {
        let g = self.inner.lock().unwrap();
        Timestamp::new(g.logical, g.now_ns)
    }
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Network delay matrix + Mars override.
#[derive(Debug, Clone)]
pub struct NetworkSimulator {
    delays: HashMap<(Body, Body), u64>,
    mars_delay_ms: Option<u64>,
}

impl NetworkSimulator {
    pub fn new() -> Self {
        NetworkSimulator {
            delays: HashMap::new(),
            mars_delay_ms: None,
        }
    }

    pub fn set_mars_delay(&mut self, delay_ms: u64) {
        self.mars_delay_ms = Some(delay_ms);
    }

    pub fn delay_ms(&self, from: Body, to: Body) -> u64 {
        if matches!(
            (from, to),
            (Body::Earth, Body::Mars)
                | (Body::Mars, Body::Earth)
                | (Body::Moon, Body::Mars)
                | (Body::Mars, Body::Moon)
        ) {
            if let Some(d) = self.mars_delay_ms {
                return d;
            }
        }
        self.delays
            .get(&(from, to))
            .copied()
            .unwrap_or_else(|| default_delay_ms(from, to))
    }

    /// Apply delay to the virtual clock (simulates waiting for RTT/2 one-way).
    pub fn apply_one_way(&self, clock: &VirtualClock, from: Body, to: Body) {
        clock.advance_ms(self.delay_ms(from, to));
    }
}

impl Default for NetworkSimulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Chaos injector for bit flips and node loss.
#[derive(Debug, Clone, Default)]
pub struct ChaosInjector {
    bit_flips: Vec<(String, String)>, // (node_id, block_id)
    lost_nodes: Vec<String>,
}

impl ChaosInjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inject_bit_flip(&mut self, node_id: impl Into<String>, block_id: impl Into<String>) {
        self.bit_flips.push((node_id.into(), block_id.into()));
    }

    pub fn lose_node(&mut self, node_id: impl Into<String>) {
        self.lost_nodes.push(node_id.into());
    }

    pub fn is_node_lost(&self, node_id: &str) -> bool {
        self.lost_nodes.iter().any(|n| n == node_id)
    }

    pub fn pending_bit_flips(&self) -> &[(String, String)] {
        &self.bit_flips
    }

    pub fn take_bit_flips(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.bit_flips)
    }
}

/// Scenario definition for multi-step simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub nodes: Vec<ScenarioNode>,
    pub clock_rate: f64,
    pub mars_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioNode {
    pub id: String,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulationResult {
    pub name: String,
    pub steps: u64,
    pub final_logical: u64,
    pub ok: bool,
    pub messages: Vec<String>,
}

/// Top-level harness environment.
#[derive(Debug, Clone)]
pub struct SimulationEnv {
    pub clock: VirtualClock,
    pub network: NetworkSimulator,
    pub chaos: ChaosInjector,
    pub scenario: Scenario,
}

impl SimulationEnv {
    pub fn from_scenario(scenario: Scenario) -> Self {
        let clock = VirtualClock::new();
        clock.set_rate(scenario.clock_rate);
        let mut network = NetworkSimulator::new();
        if let Some(d) = scenario.mars_delay_ms {
            network.set_mars_delay(d);
        }
        SimulationEnv {
            clock,
            network,
            chaos: ChaosInjector::new(),
            scenario,
        }
    }

    pub fn inject_bit_flip(&mut self, node_id: impl Into<String>, block_id: impl Into<String>) {
        self.chaos.inject_bit_flip(node_id, block_id);
    }

    pub fn set_mars_delay(&mut self, delay_ms: u64) {
        self.network.set_mars_delay(delay_ms);
    }

    pub fn simulate_steps(&self, steps: u64) -> SimulationResult {
        for _ in 0..steps {
            self.clock.advance_ms(1);
        }
        let ts = self.clock.timestamp();
        SimulationResult {
            name: self.scenario.name.clone(),
            steps,
            final_logical: ts.logical,
            ok: true,
            messages: vec![],
        }
    }
}

/// Flip one bit in a byte buffer (for integrity tests). No-op on empty `buf`.
pub fn flip_bit_in_buffer(buf: &mut [u8], bit_index: usize) {
    if buf.is_empty() {
        return;
    }
    let byte_i = (bit_index / 8) % buf.len();
    let bit = bit_index % 8;
    buf[byte_i] ^= 1 << bit;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mars_delay_override() {
        let mut net = NetworkSimulator::new();
        assert_eq!(net.delay_ms(Body::Earth, Body::Mars), 240_000);
        net.set_mars_delay(500);
        assert_eq!(net.delay_ms(Body::Earth, Body::Mars), 500);
    }

    #[test]
    fn clock_advances() {
        let clock = VirtualClock::new();
        clock.advance_ms(10);
        assert_eq!(clock.now_ns(), 10_000_000);
        assert_eq!(clock.timestamp().logical, 1);
    }

    #[test]
    fn bit_flip_changes_buffer() {
        let mut buf = vec![0u8; 4];
        flip_bit_in_buffer(&mut buf, 0);
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn scenario_env_runs() {
        let sc = Scenario {
            name: "smoke".into(),
            nodes: vec![ScenarioNode {
                id: "n1".into(),
                body: Body::Earth,
            }],
            clock_rate: 1.0,
            mars_delay_ms: None,
        };
        let env = SimulationEnv::from_scenario(sc);
        let r = env.simulate_steps(5);
        assert!(r.ok);
        assert_eq!(r.steps, 5);
    }
}
