//! 生命周期状态机：Created → Starting → Started → Stopping → Stopped / Disposed / Failed。

/// 组件（Scope / Service）生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LifecycleState {
    Created = 0,
    Starting = 1,
    Started = 2,
    Stopping = 3,
    Stopped = 4,
    Disposed = 5,
    Failed = 6,
}

impl LifecycleState {
    pub fn from_u8(v: u8) -> LifecycleState {
        match v {
            0 => LifecycleState::Created,
            1 => LifecycleState::Starting,
            2 => LifecycleState::Started,
            3 => LifecycleState::Stopping,
            4 => LifecycleState::Stopped,
            5 => LifecycleState::Disposed,
            _ => LifecycleState::Failed,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LifecycleState::Created => "created",
            LifecycleState::Starting => "starting",
            LifecycleState::Started => "started",
            LifecycleState::Stopping => "stopping",
            LifecycleState::Stopped => "stopped",
            LifecycleState::Disposed => "disposed",
            LifecycleState::Failed => "failed",
        }
    }

    /// 是否处于运行中的过渡/活动状态。
    pub fn is_running(&self) -> bool {
        matches!(self, LifecycleState::Starting | LifecycleState::Started)
    }
}

/// 生命周期相关事件名。
pub mod lifecycle_events {
    pub const STARTING: &str = "lifecycle:starting";
    pub const STARTED: &str = "lifecycle:started";
    pub const STOPPING: &str = "lifecycle:stopping";
    pub const SERVICE_STARTED: &str = "service:started";
    pub const SERVICE_STOPPED: &str = "service:stopped";
    pub const SCOPE_DISPOSED: &str = "scope:disposed";
}
