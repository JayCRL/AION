//! 启动器：组装 AdapterKit、系统服务与 Cordis 运行时。

use std::path::PathBuf;

use aion_adapter::AdapterKit;
use cordis::prelude::*;

/// cgroup v2 挂载点（Linux）；其他平台使用临时目录占位（模拟模式）。
pub fn default_cgroup_root() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/sys/fs/cgroup")
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::temp_dir().join("aion-cgroup-emulated")
    }
}

/// StorageService 默认根目录。
pub fn default_storage_root() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("var")
        .join("storage")
}

/// 加载可选配置文件 `aion.json`（不存在则为空配置）。
pub fn load_config() -> Config {
    Config::load_optional("aion.json")
}

/// 解析配置中的日志级别。
pub fn log_level_from(config: &Config) -> Level {
    config
        .get_string("log.level")
        .and_then(|s| Level::parse(&s))
        .unwrap_or(Level::Info)
}

/// 启动 AION 运行时：核心层（Cordis）→ 服务层（System Services）就绪。
pub async fn boot(config: Config) -> anyhow::Result<Context> {
    let kit = AdapterKit::native(default_cgroup_root());
    let storage_root = config
        .get_string("storage.root")
        .map(PathBuf::from)
        .unwrap_or_else(default_storage_root);
    let cgroup_root = default_cgroup_root();

    let level = log_level_from(&config);
    let app = App::new()
        .config(config)
        .log_level(level)
        .plugin(plugin_fn("aion:system", move |ctx: Context| {
            // 插件在 apply 时构建服务（保证闭包满足 Fn 约束）
            let kit = kit.clone();
            let storage_root = storage_root.clone();
            let cgroup_root = cgroup_root.clone();
            Box::pin(async move {
                // 服务层：基于 Cordis-RS Service 的 AION System Services
                let services = aion_services::system_services(&kit, storage_root, cgroup_root);
                aion_services::provide_all(&ctx, services)?;
                Ok(())
            })
        }));
    Ok(app.run().await?)
}
