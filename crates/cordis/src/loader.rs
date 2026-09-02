//! 加载器 / 分组：`App` 构建器组装配置、日志与插件，产出运行中的根 Context。

use std::path::Path;
use std::sync::Arc;

use crate::config::Config;
use crate::context::Context;
use crate::logger::{Level, Logger};
use crate::plugin::Plugin;
use crate::{CordisError, CordisResult};

/// 应用构建器：声明配置与插件，`run()` 后得到根上下文。
pub struct App {
    config: Config,
    level: Level,
    plugins: Vec<Arc<dyn Plugin>>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        App {
            config: Config::new(),
            level: Level::Info,
            plugins: Vec::new(),
        }
    }

    /// 直接指定配置。
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// 合并一份 JSON 配置文件（文件必须存在）。
    pub fn config_file(self, path: impl AsRef<Path>) -> CordisResult<Self> {
        let extra = Config::load_file(path)?;
        let mut config = self.config;
        config.merge(&extra);
        Ok(Self { config, ..self })
    }

    /// 合并一份可选 JSON 配置文件（不存在则忽略）。
    pub fn config_file_optional(self, path: impl AsRef<Path>) -> Self {
        let extra = Config::load_optional(path);
        let mut config = self.config;
        config.merge(&extra);
        Self { config, ..self }
    }

    /// 设置日志级别。
    pub fn log_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// 注册插件（按注册顺序加载）。
    pub fn plugin<P: Plugin + 'static>(mut self, plugin: P) -> Self {
        self.plugins.push(Arc::new(plugin));
        self
    }

    /// 注册插件（Arc 形式）。
    pub fn plugin_arc(mut self, plugin: Arc<dyn Plugin>) -> Self {
        self.plugins.push(plugin);
        self
    }

    /// 运行：创建根上下文并依序加载插件；任一插件失败则整体销毁并返回错误。
    pub async fn run(self) -> CordisResult<Context> {
        let logger = Logger::new(self.level);
        let ctx = Context::root(self.config, logger);
        for plugin in &self.plugins {
            if let Err(e) = ctx.plugin_arc(plugin.clone()).await {
                let _ = ctx.dispose().await;
                return Err(CordisError::PluginFailed(
                    plugin.name().to_string(),
                    e.to_string(),
                ));
            }
        }
        Ok(ctx)
    }
}

/// 配置装载器：按顺序合并多份配置文件。
pub struct Loader;

impl Loader {
    /// 按顺序加载并合并多份 JSON 配置（后面的优先级高）。
    pub fn load_configs(paths: &[impl AsRef<Path>]) -> CordisResult<Config> {
        let mut config = Config::new();
        for path in paths {
            let extra = Config::load_file(path)?;
            config.merge(&extra);
        }
        Ok(config)
    }

    /// 仅加载存在的文件，忽略缺失项。
    pub fn load_configs_optional(paths: &[impl AsRef<Path>]) -> Config {
        let mut config = Config::new();
        for path in paths {
            config.merge(&Config::load_optional(path));
        }
        config
    }
}
