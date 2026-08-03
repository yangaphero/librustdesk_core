use hbb_common::{
    anyhow::{bail, Result},
    config::{self, Config},
    ResultType,
};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Data {
    SwitchSidesRequest(String),
    SwitchSidesUuid(String, String, Option<bool>),
    SyncWinCpuUsage(Option<f32>),
}

pub struct Connection;

impl Connection {
    pub async fn send(&mut self, _data: &Data) -> ResultType<()> {
        bail!("OHOS IPC service is not available")
    }

    pub async fn next_timeout(&mut self, _timeout: u64) -> ResultType<Option<Data>> {
        Ok(None)
    }
}

pub async fn connect(_ms_timeout: u64, _postfix: &str) -> ResultType<Connection> {
    bail!("OHOS IPC service is not available")
}

pub fn get_socks_ws() -> (Option<config::Socks5Server>, String) {
    (
        Config::get_socks(),
        Config::get_option(config::keys::OPTION_ALLOW_WEBSOCKET),
    )
}

pub async fn get_rendezvous_server(_ms_timeout: u64) -> (String, Vec<String>) {
    (
        Config::get_rendezvous_server(),
        Config::get_rendezvous_servers(),
    )
}

pub async fn get_nat_type(_ms_timeout: u64) -> i32 {
    Config::get_nat_type()
}

pub fn test_rendezvous_server() -> Result<()> {
    Ok(())
}

pub fn send_url_scheme(_url: String) -> Result<()> {
    bail!("OHOS URL IPC is not available")
}

pub async fn get_options_async() -> HashMap<String, String> {
    Config::get_options()
}

pub fn start_pa() {}

pub fn get_hwcodec_config_from_server() -> ResultType<()> {
    Ok(())
}

pub fn get_id() -> String {
    Config::get_id()
}

pub fn set_option(key: &str, value: &str) {
    Config::set_option(key.to_owned(), value.to_owned());
}
