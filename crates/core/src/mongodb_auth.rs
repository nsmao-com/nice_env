//! 本机 MongoDB 管理认证。凭据按版本保存，不导出、不回传密码，不通过命令行传递密码。
use crate::{AppError, CoreState, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credentials { pub username: String, pub password: String, pub auth_database: String }
impl Default for Credentials {
    fn default() -> Self { Self { username: String::new(), password: String::new(), auth_database: "admin".into() } }
}
fn key(prefix: &str, version: &str) -> Result<String> {
    if version.is_empty() || version.len()>64 || !version.bytes().all(|c|c.is_ascii_alphanumeric() || b".-_".contains(&c)) {
        return Err(AppError::new("BAD_VERSION", "MongoDB 版本无效"));
    }
    Ok(format!("{prefix}@{version}"))
}
pub fn local_setting(key: &str) -> bool {
    ["mongodbCredentials@", "mongodbAuthEnabled@", "mongodbBackupPlan@"].iter().any(|prefix|key.starts_with(prefix))
}
impl Credentials {
    pub fn validate(&self) -> Result<()> {
        if self.username.len()>256 || self.password.len()>4096 || self.username.chars().any(char::is_control)
            || self.password.chars().any(char::is_control) || self.username.is_empty()!=self.password.is_empty()
            || self.auth_database.is_empty() || self.auth_database.len()>63
            || self.auth_database.chars().any(|c| c.is_control() || c.is_whitespace() || "/\\.\"$*<>:|?".contains(c)) {
            return Err(AppError::new("MONGO_CREDENTIALS_INVALID", "请填写有效的用户名、密码和认证数据库；无认证时用户名及密码均留空"));
        }
        Ok(())
    }
    pub(crate) fn load(state: &CoreState, version: &str) -> Result<Self> {
        let credentials = match state.store.get_setting_checked(&key("mongodbCredentials",version)?)? {
            Some(raw) => serde_json::from_str(&raw).map_err(|_|AppError::new("MONGO_CREDENTIALS_INVALID", "本机 MongoDB 连接记录损坏，请重新保存连接认证"))?,
            None => Self::default(),
        };
        credentials.validate()?; Ok(credentials)
    }
    pub(crate) fn redact(&self, text: &str) -> String {
        if self.password.is_empty() { return text.into(); }
        let escaped = serde_json::to_string(&self.password).unwrap_or_default();
        text.replace(&self.password,"[redacted]").replace(&escaped[1..escaped.len()-1],"[redacted]")
    }
}
pub(crate) fn enabled(store: &crate::store::Store, version: &str) -> Result<bool> {
    match store.get_setting_checked(&key("mongodbAuthEnabled",version)?)?.as_deref() {
        None | Some("false") => Ok(false), Some("true") => Ok(true),
        _ => Err(AppError::new("MONGO_AUTH_CONFIG", "MongoDB 认证设置损坏，已停止启动以避免意外关闭认证")),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthView {
    pub version: String, pub username: String, pub auth_database: String, pub has_password: bool,
    pub configured: bool, pub running: bool, pub authorization: Option<bool>, pub has_users: Option<bool>,
    pub administrator: bool, pub problem: Option<AppError>, pub revision: String,
}
const PROBE: &str = r#"
  const auth = checked(admin.runCommand({connectionStatus:1})).authInfo;
  let hasUsers = null;
  try { hasUsers = checked(admin.runCommand({usersInfo:{forAllDBs:true},showCredentials:false})).users.length > 0; } catch (error) { if (error.code !== 13) throw error; }
  print(JSON.stringify({result:{authorization:options.parsed?.security?.authorization === 'enabled',hasUsers,
    administrator:(auth.authenticatedUserRoles || []).some(r=>r.db==='admin' && r.role==='root')}}));
"#;
fn probe(state: &CoreState, version: &str, credentials: &Credentials) -> Result<Value> {
    crate::mongodb::execute_as(state,version,json!({}),PROBE,credentials)
}
pub fn status(state: &CoreState, version: &str) -> Result<AuthView> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(||AppError::new("SERVICE_BUSY","服务正在操作，请稍后刷新认证状态"))?;
    state.store.find_installed("mongodb",Some(version)).ok_or_else(||AppError::not_installed("MongoDB"))?;
    let (credentials, credential_problem) = match Credentials::load(state,version) { Ok(value)=>(value,None),Err(error)=>(Credentials::default(),Some(error)) };
    let service = state.manager.snapshot("mongodb");
    let running = service.as_ref().is_some_and(|s|s.version.as_deref()==Some(version) && s.pids.iter().any(|pid|platform::process_alive(*pid)));
    let mut view = AuthView { version:version.into(), username:credentials.username.clone(), auth_database:credentials.auth_database.clone(),
        has_password:!credentials.password.is_empty(),configured:enabled(&state.store,version)?,running,authorization:None,has_users:None,administrator:false,problem:credential_problem,revision:String::new() };
    if running && view.problem.is_none() {
        match probe(state,version,&credentials) {
            Ok(value)=>{ view.authorization=value["authorization"].as_bool(); view.has_users=value["hasUsers"].as_bool(); view.administrator=value["administrator"]==true; },
            Err(error)=>view.problem=Some(error),
        }
    }
    // 修订号只暴露摘要，绑定凭据、运行进程、实际认证状态与账号存在情况。
    view.revision=hex::encode(Sha256::digest(serde_json::to_vec(&json!({"version":version,"configured":view.configured,
        "running":running,"authorization":view.authorization,"hasUsers":view.has_users,"administrator":view.administrator,
        "service":service.as_ref().map(|s|json!({"version":s.version,"port":s.port,"pids":s.pids,"state":s.state})),
        "credentials":state.store.get_setting_checked(&key("mongodbCredentials",version)?)?}))
        .map_err(|e|AppError::internal("读取 MongoDB 认证状态",e.to_string()))?));
    Ok(view)
}
fn current(state: &CoreState, version: &str, revision: &str) -> Result<AuthView> {
    let view=status(state,version)?;
    if revision!=view.revision { return Err(AppError::new("MONGO_AUTH_CHANGED","实例、账号或认证设置已变化，请刷新后重新确认")); }
    if !view.running { return Err(AppError::new("MONGO_NOT_RUNNING","请先启动所选 MongoDB，再验证或修改认证")); }
    Ok(view)
}
pub fn save_connection(state: &CoreState, version: &str, revision: &str, credentials: Credentials) -> Result<AuthView> {
    let _work=crate::BackgroundWork::begin("验证 MongoDB 连接认证")?;
    let _activity=crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock=state.manager.lifecycle.try_lock().ok_or_else(||AppError::new("SERVICE_BUSY","服务正在操作，请稍后验证连接"))?;
    let before=current(state,version,revision)?; credentials.validate()?;
    // 配置已开启但本机凭据记录损坏或旧密码失效时，允许用图形入口重新验证并替换记录；
    // 配置仍是无认证时继续拒绝带账号连接，避免把凭据保存到未受保护的实例。
    if !credentials.username.is_empty() && before.authorization != Some(true) && !before.configured {
        return Err(AppError::new("MONGO_AUTH_DISABLED", "实例尚未开启认证，不能验证用户名密码；请先初始化管理员并开启认证"));
    }
    probe(state,version,&credentials)?;
    state.store.set_setting_json(&key("mongodbCredentials",version)?,&credentials)?;
    status(state,version)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyAuth {
    pub revision: String, pub enabled: bool, pub acknowledge_restart: bool, pub acknowledge_disable: bool,
    pub administrator: Option<Credentials>,
}
pub fn apply(state: &CoreState, version: &str, input: ApplyAuth) -> Result<AuthView> {
    let _work=crate::BackgroundWork::begin("应用 MongoDB 访问认证")?;
    let _activity=crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock=state.manager.lifecycle.try_lock().ok_or_else(||AppError::new("SERVICE_BUSY","服务正在操作，请等待完成后再修改认证"))?;
    let before=current(state,version,&input.revision)?;
    if !input.acknowledge_restart || (!input.enabled && !input.acknowledge_disable) {
        return Err(AppError::new("MONGO_AUTH_CONFIRM","请确认重启会中断连接；关闭认证还需确认本机客户端可直接访问数据"));
    }
    if crate::ops::installed_by_choice(&state.store,"mongodb").is_none_or(|p|p.version!=version) {
        return Err(AppError::new("MONGO_AUTH_CHANGED","默认 MongoDB 版本已变化，请切换回当前版本后重试"));
    }
    if let Some(error)=before.problem { return Err(error); }
    if let Some(credentials)=input.administrator {
        credentials.validate()?;
        if !input.enabled || before.authorization!=Some(false) || before.has_users!=Some(false)
            || credentials.username.trim().is_empty() || credentials.password.chars().count()<8 || credentials.auth_database!="admin" {
            return Err(AppError::new("MONGO_ADMIN_EXISTS","仅无账号且未启用认证的实例可初始化管理员；已有账号请先验证现有管理员连接"));
        }
        let previous=Credentials::load(state,version)?;
        let credential_key=key("mongodbCredentials",version)?;
        // 创建前保存，避免创建成功后异常退出导致本机丢失管理密码。
        state.store.set_setting_json(&credential_key,&credentials)?;
        let created=crate::mongodb::execute_as(state,version,json!({"username":credentials.username,"password":credentials.password}),r#"
          if (checked(admin.runCommand({usersInfo:{forAllDBs:true}})).users.length) throw Object.assign(new Error('Existing users'),{code:'MONGO_ADMIN_EXISTS'});
          checked(admin.runCommand({createUser:input.request.username,pwd:input.request.password,roles:[{role:'root',db:'admin'}]}));
          print(JSON.stringify({result:true}));
        "#,&previous);
        if !probe(state,version,&credentials).is_ok_and(|v|v["administrator"]==true) {
            if created.as_ref().is_err_and(|error|error.code=="MONGO_ADMIN_EXISTS") { state.store.set_setting_json(&credential_key,&previous)?; }
            return Err(created.err().unwrap_or_else(||AppError::new("MONGO_ADMIN_UNVERIFIED","管理员可能已创建，但新连接尚未验证，请用刚才的账号密码重新验证连接"))
                .with_hint("未启用认证；请检查管理员是否已创建。本机保留刚才的连接记录，可通过验证连接修复"));
        }
    }
    let credentials=Credentials::load(state,version)?;
    if credentials.username.is_empty() || probe(state,version,&credentials)?["administrator"]!=true {
        return Err(AppError::new("MONGO_ADMIN_REQUIRED","请先验证具有 admin 数据库 root 角色的管理账号，再修改访问认证"));
    }
    state.store.set_setting(&key("mongodbAuthEnabled",version)?,if input.enabled {"true"} else {"false"})?;
    // 重启失败保留认证配置和凭据；绝不自动回退为无认证启动。
    state.restart_service("mongodb").map_err(|error|error.with_hint("认证配置和本机凭据已保存。请检查服务状态，排除启动问题后重新启动；不会自动改为无认证"))?;
    let after=status(state,version)?;
    if after.problem.is_some() || after.authorization!=Some(input.enabled) {
        return Err(AppError::new("MONGO_AUTH_UNVERIFIED","重启后的认证状态尚未验证，请刷新状态并检查连接记录").with_hint("配置和凭据已保留，未自动关闭认证"));
    }
    Ok(after)
}

pub fn change_password(state: &CoreState, version: &str, revision: &str, password: String) -> Result<AuthView> {
    let _work=crate::BackgroundWork::begin("修改 MongoDB 管理密码")?;
    let _activity=crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock=state.manager.lifecycle.try_lock().ok_or_else(||AppError::new("SERVICE_BUSY","服务正在操作，请稍后修改密码"))?;
    let before=current(state,version,revision)?;
    if !before.administrator { return Err(AppError::new("MONGO_ADMIN_REQUIRED","请先验证管理账号连接")); }
    let previous=Credentials::load(state,version)?;
    let candidate=Credentials { password,..previous.clone() }; candidate.validate()?;
    if candidate.password.chars().count()<8 { return Err(AppError::new("MONGO_PASSWORD_SHORT","新密码至少需要 8 个字符")); }
    let credential_key=key("mongodbCredentials",version)?;
    state.store.set_setting_json(&credential_key,&candidate)?;
    let changed=crate::mongodb::execute_as(state,version,json!({"password":candidate.password}),r#"
      checked(connection.getDB(input.credentials.authDatabase).runCommand({updateUser:input.credentials.username,pwd:input.request.password}));
      print(JSON.stringify({result:true}));
    "#,&previous);
    if probe(state,version,&candidate).is_ok_and(|v|v["administrator"]==true) { return status(state,version); }
    if probe(state,version,&previous).is_ok() { state.store.set_setting_json(&credential_key,&previous)?; }
    Err(changed.err().unwrap_or_else(||AppError::new("MONGO_PASSWORD_UNVERIFIED","密码修改结果尚未确认，请使用当前密码重新验证连接")))
}
