"use client";
import { Suspense } from "react";
import { ConfigEditor } from "@/components/shared/config-editor";
import { PageHeader } from "@/components/layout/app-shell";
import { useT } from "@/lib/store";
export default function Page() {
  const t = useT();
  return <div className="pb-8">
    <PageHeader title={t("nav.configuration")} subtitle={t("cfgeditor.subtitle")} />
    <Suspense fallback={<p role="status" className="py-4 text-sm text-muted">{t("common.loading")}</p>}>
      <ConfigEditor />
    </Suspense>
  </div>;
}
