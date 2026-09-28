"use client";

import { Settings2 } from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { useT } from "@/lib/store";

/**
 * 只为已有真实配置编辑器的服务显示入口。
 * 版本号保留在 service id 中，配置页会据此打开对应版本的配置文件。
 */
const CONFIGURABLE_SERVICES = new Set([
  "nginx",
  "php",
  "mysql",
  "mariadb",
  "redis",
  "apache",
  "mihomo",
  "postgresql",
  "mongodb",
]);

export function ServiceConfigButton({ service, disabled = false }: { service: ServiceStatus; disabled?: boolean }) {
  const t = useT();
  const router = useRouter();
  const baseId = service.id.split("@")[0];
  if (!CONFIGURABLE_SERVICES.has(baseId)) return null;

  const label = t("svc.config");
  return (
    <Button
      type="button"
      variant="ghost"
      size="sm"
      className="min-h-8 shrink-0 gap-1.5 px-2 text-[11px] text-primary"
      disabled={disabled}
      aria-label={`${label} · ${service.label}`}
      title={label}
      onClick={() => {
        const target = service.version && !service.id.includes("@") ? `${service.id}@${service.version}` : service.id;
        router.push(`/configuration?service=${encodeURIComponent(target)}`);
      }}
    >
      <Settings2 className="h-3.5 w-3.5 shrink-0" />
      {label}
    </Button>
  );
}
