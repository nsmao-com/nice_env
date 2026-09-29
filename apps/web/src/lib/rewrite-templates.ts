import type { CustomRewrite } from "@nsb/schema";

export type RewriteTemplate = CustomRewrite & {
  category: "PHP 框架" | "内容管理" | "静态站点";
  description: string;
  documentRoot: string;
  source: string;
};
type TemplateDefinition = Omit<RewriteTemplate, "server" | "content"> & {
  rules: Record<CustomRewrite["server"], string>;
};

const nginxPhp = `location / {
    try_files $uri $uri/ /index.php?$query_string;
}`;
// PHP snippets run in VirtualHost, static snippets in Directory. Use the
// document root because REQUEST_FILENAME may not yet be mapped in VirtualHost.
const apachePhp = `RewriteEngine On
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-f
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-d
RewriteRule ^ /index.php [QSA,END]`;
const caddyPhp = `try_files {path} {path}/ /index.php?{query}`;
const phpRules = { nginx: nginxPhp, apache: apachePhp, caddy: caddyPhp };
const staticRules = {
  nginx: `location / {
    try_files $uri $uri.html $uri/ =404;
}`,
  apache: `RewriteEngine On
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-f
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-d
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI}.html -f
RewriteRule ^(.+?)/?$ $1.html [END]`,
  caddy: `try_files {path} {path}.html {path}/`,
};

const definitions: TemplateDefinition[] = [
  { name: "Laravel", category: "PHP 框架", documentRoot: "public", description: "请求交给 index.php，保留查询参数。", source: "https://laravel.com/docs/deployment", rules: phpRules },
  { name: "Symfony", category: "PHP 框架", documentRoot: "public", description: "适用于使用 public/index.php 的 Symfony 项目。", source: "https://symfony.com/doc/current/setup/web_server_configuration.html", rules: phpRules },
  { name: "ThinkPHP", category: "PHP 框架", documentRoot: "public", description: "ThinkPHP 5 / 6 / 8，通过 s 参数传递路由。", source: "https://www.kancloud.cn/manual/thinkphp6_0/1037488", rules: {
    nginx: `location / {
    if (!-e $request_filename) {
        rewrite ^/(.*)$ /index.php?s=/$1 last;
    }
}`,
    apache: `RewriteEngine On
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-f
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-d
RewriteRule ^/?(.*)$ /index.php?s=/$1 [QSA,END]`,
    caddy: `@thinkphp_missing not file {path} {path}/
rewrite @thinkphp_missing /index.php?s={path}&{query}`,
  } },
  { name: "WordPress", category: "内容管理", documentRoot: "WordPress 安装目录", description: "单站点固定链接；不适用于 Multisite 子目录模式。", source: "https://developer.wordpress.org/advanced-administration/server/web-server/nginx/", rules: {
    ...phpRules,
    nginx: `location = /wp-admin {
    return 301 /wp-admin/$is_args$args;
}
${nginxPhp}`,
    caddy: `redir /wp-admin /wp-admin/?{query} 301
${caddyPhp}`,
  } },
  { name: "Yii2", category: "PHP 框架", documentRoot: "web（高级模板为 frontend/web）", description: "需在 urlManager 启用 enablePrettyUrl 并关闭 showScriptName。", source: "https://www.yiiframework.com/doc/guide/2.0/en/start-installation", rules: phpRules },
  { name: "CodeIgniter 4", category: "PHP 框架", documentRoot: "public", description: "隐藏 index.php；应用的 indexPage 设置为空。", source: "https://codeigniter.com/user_guide/general/urls.html", rules: phpRules },
  { name: "CakePHP", category: "PHP 框架", documentRoot: "webroot", description: "CakePHP 4 / 5 入口路由；不使用旧版 url 查询参数。", source: "https://book.cakephp.org/5/en/installation.html", rules: phpRules },
  { name: "Drupal", category: "内容管理", documentRoot: "web（Composer 项目）", description: "Drupal 10 / 11 简洁网址，并阻止访问常见内部文件。", source: "https://www.drupal.org/docs/getting-started/system-requirements/web-server-requirements", rules: {
    nginx: String.raw`location ~* \.(engine|inc|info|install|module|profile|po|sh|sql|theme|tpl(\.php)?|xtmpl|yml)$ {
    deny all;
}
location ^~ /sites/default/files/private/ {
    deny all;
}
${nginxPhp}`,
    apache: String.raw`RewriteEngine On
RewriteRule \.(engine|inc|info|install|module|profile|po|sh|sql|theme|tpl(\.php)?|xtmpl|yml)$ - [F,END,NC]
RewriteRule ^/?sites/default/files/private/ - [F,END,NC]
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-f
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-d
RewriteRule ^ /index.php [QSA,END]`,
    caddy: String.raw`@drupal_private path_regexp drupal_private (?i)\.(engine|inc|info|install|module|profile|po|sh|sql|theme|tpl(\.php)?|xtmpl|yml)$
respond @drupal_private 403
@drupal_files path /sites/default/files/private/*
respond @drupal_files 403
${caddyPhp}`,
  } },
  { name: "Joomla", category: "内容管理", documentRoot: "Joomla 安装目录", description: "在全局设置中同时启用 SEF 网址和 URL 重写。", source: "https://guide.joomla.org/user-manual/seo/seo-sef-urls-on-nginx", rules: phpRules },
  { name: "Slim 4", category: "PHP 框架", documentRoot: "public", description: "适用于以 public/index.php 为入口的 Slim API 或网站。", source: "https://www.slimframework.com/docs/v4/start/web-servers.html", rules: { ...phpRules, nginx: `location / {
    try_files $uri /index.php$is_args$args;
}` } },
  { name: "Flarum", category: "内容管理", documentRoot: "public", description: "论坛路由；使用官方 public 目录布局。", source: "https://github.com/flarum/flarum/blob/2.x/.nginx.conf", rules: phpRules },
  { name: "Typecho", category: "内容管理", documentRoot: "Typecho 安装目录", description: "博客路由；需在永久链接设置中启用地址重写。", source: "https://docs.typecho.org/servers", rules: phpRules },
  { name: "Craft CMS", category: "内容管理", documentRoot: "web", description: "将不存在的文件请求交给入口；Apache / Caddy 使用 p 路径参数。", source: "https://craftcms.com/docs/5.x/requirements.html", rules: {
    nginx: nginxPhp,
    apache: `RewriteEngine On
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-f
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-d
RewriteRule ^/?(.*)$ /index.php?p=$1 [QSA,END]`,
    caddy: `@craft_missing not file {path} {path}/
rewrite @craft_missing /index.php?p={path}&{query}`,
  } },
  { name: "Statamic", category: "内容管理", documentRoot: "public", description: "基于 Laravel 的内容站点，保留静态资源直接访问。", source: "https://statamic.dev/deploying", rules: phpRules },
  { name: "PHP 通用入口", category: "PHP 框架", documentRoot: "包含 index.php 的公开目录", description: "适用于从 REQUEST_URI 读取路由的前端控制器。", source: "https://nginx.org/en/docs/http/ngx_http_core_module.html#try_files", rules: phpRules },
  { name: "SPA fallback", category: "静态站点", documentRoot: "dist / build", description: "Vue Router、React Router 等 history 路由回退到 index.html。", source: "https://router.vuejs.org/guide/essentials/history-mode.html", rules: {
    nginx: `location / {
    try_files $uri $uri/ /index.html;
}`,
    apache: `RewriteEngine On
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-f
RewriteCond %{DOCUMENT_ROOT}%{REQUEST_URI} !-d
RewriteRule ^ /index.html [END]`,
    caddy: `try_files {path} {path}/ /index.html`,
  } },
  { name: "Next.js export", category: "静态站点", documentRoot: "out", description: "静态导出：查找 .html 和目录首页，不适用于 SSR 服务。", source: "https://nextjs.org/docs/app/guides/static-exports", rules: staticRules },
  { name: "Nuxt generate", category: "静态站点", documentRoot: ".output/public", description: "预渲染输出的文件和目录首页；未生成的路径返回 404。", source: "https://nuxt.com/docs/getting-started/deployment", rules: staticRules },
  { name: "VitePress", category: "静态站点", documentRoot: ".vitepress/dist", description: "支持 cleanUrls 无后缀链接，缺失页面保留 404。", source: "https://vitepress.dev/guide/deploy", rules: staticRules },
  { name: "Hugo / Jekyll", category: "静态站点", documentRoot: "public / _site", description: "静态博客的目录首页与 HTML 文件，不将所有请求回退首页。", source: "https://gohugo.io/host-and-deploy/", rules: staticRules },
];

/** Shared by the library and site selector. Sites still store their own rule snapshots. */
export const REWRITE_TEMPLATES: RewriteTemplate[] = definitions.flatMap(({ rules, ...details }) =>
  (Object.keys(rules) as CustomRewrite["server"][]).map((server) => ({ ...details, server, content: rules[server] })),
);
