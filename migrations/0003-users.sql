-- 用户表：内置账号，无注册功能；密码为 sha256("用户名:密码") 的十六进制
CREATE TABLE IF NOT EXISTS users (
	username      TEXT PRIMARY KEY,
	display_name  TEXT NOT NULL DEFAULT '',
	password_hash TEXT NOT NULL,
	created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 便签记录最后编辑者
ALTER TABLE notes ADD COLUMN author TEXT NOT NULL DEFAULT '';

-- 内置用户（INSERT OR IGNORE 可重复执行）
INSERT OR IGNORE INTO users (username, display_name, password_hash) VALUES
	('admin', '管理员', 'efece4e87c16d55378975e996e5ac989ae74ef1995abba6e0493ea080f828068'),
	('guest', '访客',   'c669ef97e3555f4811d659da5036b28ab30ee8daec80333cb15a09e8ceee4690'),
	('amigo', 'Amigo',  '1cc481da1004eb37fe31618cbb5718c261b13c876884c51476c812ec665bc105');
