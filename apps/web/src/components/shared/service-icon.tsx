"use client";

import * as React from "react";
import {
  siAdminer,
  siApache,
  siApachetomcat,
  siBun,
  siCaddy,
  siCloudflare,
  siComposer,
  siConsul,
  siDeno,
  siDotnet,
  siEclipseadoptium,
  siElasticsearch,
  siErlang,
  siEtcd,
  siFlutter,
  siGo,
  siGradle,
  siMariadb,
  siMeilisearch,
  siMinio,
  siMongodb,
  siMysql,
  siNeo4j,
  siNginx,
  siNodedotjs,
  siOllama,
  siPerl,
  siPhp,
  siPostgresql,
  siPython,
  siQdrant,
  siRabbitmq,
  siRedis,
  siRuby,
  siRust,
  siRustfs,
  siTemporal,
  siZig,
  siZincsearch,
  type SimpleIcon,
} from "simple-icons";
import { Server } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * 服务品牌图标 —— 一律用真实 logo，不用通用图标顶替：
 * · Simple Icons（simpleicons.org，随包离线打包）里收录的品牌，直接用
 *   官方矢量路径 + 品牌色；
 * · 图标库没收录的小众服务，放各自官方仓库的原版 logo（public/brands/，
 *   已裁边、透明底）；
 * · memcached 官方从未发布过 logo，是唯一回退通用图标的特例。
 * id 形如 "php" / "php@8.3"（带版本后缀），统一取 @ 前的基础名匹配。
 */
const SIMPLE_ICONS: Record<string, SimpleIcon> = {
  nginx: siNginx,
  php: siPhp,
  mysql: siMysql,
  redis: siRedis,
  adminer: siAdminer,
  apache: siApache,
  node: siNodedotjs,
  python: siPython,
  go: siGo,
  postgresql: siPostgresql,
  mongodb: siMongodb,
  composer: siComposer,
  meilisearch: siMeilisearch,
  zincsearch: siZincsearch,
  elasticsearch: siElasticsearch,
  minio: siMinio,
  rustfs: siRustfs,
  qdrant: siQdrant,
  neo4j: siNeo4j,
  consul: siConsul,
  etcd: siEtcd,
  "temporal-cli": siTemporal,
  cloudflared: siCloudflare,
  rabbitmq: siRabbitmq,
  mariadb: siMariadb,
  caddy: siCaddy,
  bun: siBun,
  deno: siDeno,
  zig: siZig,
  "temurin-jdk21": siEclipseadoptium,
  gradle: siGradle,
  ruby: siRuby,
  "ruby-devkit": siRuby,
  rust: siRust,
  "dotnet-sdk8": siDotnet,
  erlang: siErlang,
  flutter: siFlutter,
  tomcat: siApachetomcat,
  "strawberry-perl": siPerl,
  ollama: siOllama,
};

/** 本地品牌 logo。roadrunner 官方标是纯白，浅色底下不可见，固定垫深色小底。 */
const LOCAL_BRANDS: Record<string, { src: string; title: string; dark?: boolean }> = {
  mihomo: { src: "/brands/mihomo.png", title: "mihomo" },
  frankenphp: { src: "/brands/frankenphp.png", title: "FrankenPHP" },
  mailpit: { src: "/brands/mailpit.svg", title: "Mailpit" },
  rnacos: { src: "/brands/rnacos.png", title: "r-nacos" },
  coredns: { src: "/brands/coredns.png", title: "CoreDNS" },
  sftpgo: { src: "/brands/sftpgo.png", title: "SFTPGo" },
  roadrunner: { src: "/brands/roadrunner.png", title: "RoadRunner", dark: true },
};

/** 亮度 <0.28 的深色标（bun/rust/gradle 这类纯黑 logo）在深色模式下转白；
    >0.72 的浅色标（Tomcat 黄）在浅色模式下略压暗，保证贴底可辨。 */
function toneClasses(hex: string): string {
  const n = parseInt(hex, 16);
  const lum =
    (0.299 * ((n >> 16) & 0xff) + 0.587 * ((n >> 8) & 0xff) + 0.114 * (n & 0xff)) / 255;
  if (lum < 0.28) return "dark:brightness-0 dark:invert";
  if (lum > 0.72) return "brightness-90 dark:brightness-100";
  return "";
}

export function ServiceIcon({
  id,
  className,
  strokeWidth = 1.8,
}: {
  id: string;
  className?: string;
  strokeWidth?: number;
}) {
  const key = id.split("@")[0].toLowerCase();
  const icon = SIMPLE_ICONS[key];
  if (icon) {
    return (
      <svg
        role="img"
        aria-label={icon.title}
        viewBox="0 0 24 24"
        fill="currentColor"
        style={{ color: `#${icon.hex}` }}
        className={cn("shrink-0", toneClasses(icon.hex), className)}
      >
        <path d={icon.path} />
      </svg>
    );
  }
  const local = LOCAL_BRANDS[key];
  if (local) {
    return (
      <span
        className={cn(
          "inline-flex shrink-0 items-center justify-center overflow-hidden",
          local.dark && "rounded-[5px] bg-[#161616] p-[8%]",
          className
        )}
      >
        <img src={local.src} alt={local.title} title={local.title} className="h-full w-full object-contain" />
      </span>
    );
  }
  return <Server className={cn("shrink-0", className)} strokeWidth={strokeWidth} />;
}
