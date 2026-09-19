import { parseLogLine, detectLevel, tokenizeLog } from "../log-highlight";

let pass = 0, fail = 0;
function ok(cond: boolean, name: string, extra = "") {
  if (cond) { pass++; } else { fail++; console.log(`FAIL ${name} ${extra}`); }
}

// 1) nginx 错误行
const l1 = `2026-09-19 10:16:03 [error] 43#0: *1181 upstream timed out (110: Connection timed out) while reading response header`;
const p1 = parseLogLine(l1);
ok(p1.level === "error", "nginx error level", p1.level);
ok(p1.timestamp === "2026-09-19 10:16:03", "nginx timestamp", p1.timestamp);
ok(p1.tokens.map(t => t.text).join("") === l1, "nginx token concat lossless");

// 2) HTTP 状态码识别
const l2 = `2026-09-19 10:15:42 [info] 127.0.0.1 GET /index.php 200 12ms`;
const p2 = parseLogLine(l2);
ok(p2.httpStatus === 200, "http 200", String(p2.httpStatus));
const l3 = `[notice] 127.0.0.1 POST /api/x 503 3ms`;
ok(parseLogLine(l3).httpStatus === 503, "http 503");
ok(parseLogLine(l3).level === "notice", "notice level");

// 3) mysql 行
const l4 = `2026-09-18 10:02:14 [System] [MY-010931] [Server] starting as process 10600`;
const p4 = parseLogLine(l4);
ok(p4.tokens.map(t => t.text).join("") === l4, "mysql lossless", p4.tokens.map(t => t.text).join(""));

// 4) 级别优先级：fatal > error > warn
ok(detectLevel("fatal: boom") === "error", "fatal→error");
ok(detectLevel("[warn] careful") === "warn", "warn");
ok(detectLevel("nothing here") === "none", "none");

// 5) 无损：随机混合行拼接必须等于原文
const samples = [
  l1, l2, l3, l4,
  `127.0.0.1 - - [19/Sep/2026:10:00:00 +0800] "GET /a/b?c=1 HTTP/1.1" 404 512 "-" "curl/8.0"`,
  `2026-09-19T10:00:00.123Z level=info msg="server started" port=8080`,
  `[2026-09-19 10:00:00] [INFO] key=value 耗时 12.5ms`,
  ``,
  `   `,
  `plain text without any structure`,
];
for (const s of samples) {
  const joined = tokenizeLog(s).map(t => t.text).join("");
  ok(joined === s, `lossless: ${JSON.stringify(s.slice(0, 40))}`, JSON.stringify(joined.slice(0, 40)));
}

// 6) 空行不崩
ok(parseLogLine("").tokens.length === 1, "empty line one token");

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) { throw new Error(fail + " checks failed"); }
