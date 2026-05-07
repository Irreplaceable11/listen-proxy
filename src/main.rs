use std::fs;
use anyhow::Result;
use tracing::debug;
use crate::config::MainConfig;

mod config;

#[tokio::main]
async fn main() -> Result<()> {
    let str_content = fs::read_to_string("proxy-config.toml")?;


    let config = match toml::from_str::<MainConfig>(&str_content) {
        Ok(config) => {
            debug!("✅ 配置解析成功！\n");
            debug!("{:#?}", config);
            config
        }
        Err(err) => {
            debug!("❌ 配置解析失败: {}", err);
            panic!("❌ 配置解析失败: {}", err);
        }
    };
    
    Ok(())
}
