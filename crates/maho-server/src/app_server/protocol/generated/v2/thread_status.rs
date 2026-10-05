#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStatusNotLoaded1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStatusIdle2 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStatusSystemError3 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStatusActive4 {
    #[serde(rename = "activeFlags")]
    pub active_flags: Vec<Box<crate::app_server::protocol::generated::v2::thread_active_flag::ThreadActiveFlag>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ThreadStatus {
    #[serde(rename = "notLoaded")]
    NotLoaded(Box<ThreadStatusNotLoaded1>),
    #[serde(rename = "idle")]
    Idle(Box<ThreadStatusIdle2>),
    #[serde(rename = "systemError")]
    SystemError(Box<ThreadStatusSystemError3>),
    #[serde(rename = "active")]
    Active(Box<ThreadStatusActive4>),
}
