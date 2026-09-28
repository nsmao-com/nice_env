"use client";
import { ConfigEditor } from "@/components/shared/config-editor";
import { PageHeader } from "@/components/layout/app-shell";
import { useT } from "@/lib/store";
export default function Page() { const t = useT(); return <div className="pb-8"><PageHeader title={t("nav.configuration")} subtitle={t("cfgeditor.subtitle")} /><ConfigEditor /></div>; }
