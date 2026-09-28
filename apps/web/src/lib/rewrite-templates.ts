export const REWRITE_SNIPPETS: Record<string, string> = {
  Laravel: `location / {
    try_files $uri $uri/ /index.php?$query_string;
}`,
  Symfony: `location / {
    try_files $uri $uri/ /index.php$is_args$args;
}`,
  ThinkPHP: `location / {
    if (!-e $request_filename) {
        rewrite ^(.*)$ /index.php?s=$1 last;
    }
}`,
  WordPress: `location / {
    try_files $uri $uri/ /index.php?$args;
}
rewrite /wp-admin$ $scheme://$host$uri/ permanent;`,
  Yii2: `location / {
    try_files $uri $uri/ /index.php?$args;
}`,
  "CodeIgniter 4": `location / {
    try_files $uri $uri/ /index.php$is_args$args;
}
location ~* ^/(app|system|writable)/ {
    deny all;
}`,
  CakePHP: `location / {
    try_files $uri $uri/ /index.php?url=$uri&$args;
}`,
  Drupal: `location / {
    try_files $uri $uri/ /index.php?$query_string;
}
location ~* \.(engine|inc|info|install|module|profile|po|sh|.*sql|theme|tpl(\.php)?|xtmpl)$ {
    deny all;
}`,
  Joomla: `location / {
    try_files $uri $uri/ /index.php?$args;
}`,
  "SPA fallback": `location / {
    try_files $uri $uri/ /index.html;
}`,
  "Next.js export": `location / {
    try_files $uri $uri.html $uri/ =404;
}`,
};
