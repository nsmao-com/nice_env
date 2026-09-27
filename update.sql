-- v0.2.30：数据目录迁移时的路径更新记录。
-- 不变更表结构，不对当前源数据库执行。应用先用 SQLite VACUUM INTO
-- 生成一致性快照，再在目标暂存副本的一个事务中按主键参数化更新。
-- 以下为程序使用的语句模板记录，不是需要手工执行的部署脚本。
-- :value 由完整目录边界映射产生；外部路径、密码及历史内容不作全库替换。
-- :key / :id 为原记录主键；已是目标路径的记录不会重复更新。

-- UPDATE installed SET install_path = :value WHERE key = :key;
-- UPDATE installed SET config_path = :value WHERE key = :key;
-- UPDATE sites SET root_dir = :value WHERE id = :id;
-- UPDATE sites SET runtime = :value WHERE id = :id;
-- UPDATE sites SET php_overrides = :value WHERE id = :id;
-- UPDATE certs SET cert_path = :value WHERE id = :id;
-- UPDATE certs SET key_path = :value WHERE id = :id;
-- UPDATE cron_jobs SET command = :value WHERE id = :id;
-- UPDATE cert_automations SET data = :value WHERE id = :id;

-- sites.runtime 仅修改 cwd 和 command；保留其它 JSON 字段。
-- cert_automations.data 仅修改本地部署目标的 certPath/keyPath/script。
-- pathEnvDirs 保留原值，用于首次启动新目录时精确移除原有托管 PATH 条目。
