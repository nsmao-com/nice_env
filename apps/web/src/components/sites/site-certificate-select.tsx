"use client";

import { useQuery } from "@tanstack/react-query";
import type { SiteRuntime } from "@nsb/schema";
import * as api from "@/lib/api";
import { useT } from "@/lib/store";
import { certificateCoversDomain } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectSeparator, SelectTrigger, SelectValue } from "@/components/ui/select";

export type SiteCertificateBinding = Pick<SiteRuntime, "importedCertId" | "acmeCertId">;

export function useSiteCertificateSelection(binding: SiteCertificateBinding, domains: string[], enabled: boolean) {
  const t = useT();
  const query = useQuery({ queryKey: ["site-certificate-choices"], queryFn: api.siteCertificateChoices, enabled, retry: false, staleTime: 0 });
  const value = binding.acmeCertId ? `acme:${binding.acmeCertId}` : binding.importedCertId ? `imported:${binding.importedCertId}` : "local";
  const options = (query.data ?? []).map((cert) => {
    const missing = domains.filter((d) => !certificateCoversDomain(cert.sans, d));
    const problem = !cert.usable ? cert.problem ?? t("sites.detail.certInvalid")
      : missing.length ? `${t("sites.detail.certCoverage")} ${missing.join("、")}` : null;
    return { ...cert, value: `${cert.kind}:${cert.id}`, problem };
  });
  const selected = options.find((cert) => cert.value === value);
  const problem = value === "local" ? null : query.isError ? t("tls.readFailed")
    : query.isPending ? t("sites.detail.certLoading")
      : !selected ? t("sites.detail.certMissing") : selected.problem;
  return { query, value, options, selected, problem };
}

export function SiteCertificateSelect({ id, selection, onChange, disabled = false }: {
  id: string;
  selection: ReturnType<typeof useSiteCertificateSelection>;
  onChange: (binding: SiteCertificateBinding) => void;
  disabled?: boolean;
}) {
  const t = useT();
  const { query, value, options, selected, problem } = selection;
  return (
    <div className="flex min-w-0 flex-col gap-2">
      <Label htmlFor={id}>{t("sites.detail.certSource")}</Label>
      <Select value={value} disabled={disabled} onValueChange={(next) => onChange({
        acmeCertId: next.startsWith("acme:") ? next.slice(5) : undefined,
        importedCertId: next.startsWith("imported:") ? next.slice(9) : undefined,
      })}>
        <SelectTrigger id={id} aria-describedby={`${id}-hint${problem ? ` ${id}-error` : ""}`} aria-invalid={!!problem} className="min-w-0 [&>span]:truncate">
          <SelectValue />
        </SelectTrigger>
        <SelectContent className="w-[var(--radix-select-trigger-width)]">
          <SelectItem value="local">{t("sites.detail.localCert")}</SelectItem>
          {(["acme", "imported"] as const).map((kind) => {
            const group = options.filter((cert) => cert.kind === kind);
            return group.length > 0 && <SelectGroup key={kind}>
              <SelectSeparator />
              <SelectLabel>{t(kind === "acme" ? "sites.detail.acmeCerts" : "sites.detail.importedCerts")}</SelectLabel>
              {group.map((cert) => <SelectItem key={cert.value} value={cert.value} disabled={!!cert.problem} textValue={cert.subject} title={cert.problem ?? cert.sans.join(", ")} className="[&>span:last-child]:min-w-0 [&>span:last-child]:break-all [&>span:last-child]:whitespace-normal">
                <span>{cert.subject} · {cert.usable ? `${cert.daysLeft}d` : t("sites.detail.certInvalid")}</span>
                {cert.problem && <span className="block text-[11px] leading-relaxed">{cert.problem}</span>}
              </SelectItem>)}
            </SelectGroup>;
          })}
          {value !== "local" && !selected && <SelectItem value={value} disabled>{t("sites.detail.certUnavailableSelection")}</SelectItem>}
        </SelectContent>
      </Select>
      {query.isPending && <p id={`${id}-error`} role="status" className="text-[11px] text-muted">{t("sites.detail.certLoading")}</p>}
      {query.isError && <div id={`${id}-error`} role="alert" className="flex flex-wrap items-center gap-2 text-[11px] text-error">
        <span>{t("tls.readFailed")}</span>
        <Button type="button" size="sm" variant="ghost" disabled={disabled || query.isFetching} onClick={() => void query.refetch()}>{t("bulk.retry")}</Button>
      </div>}
      {problem && !query.isError && !query.isPending && <p id={`${id}-error`} role="alert" className="break-all text-[11px] leading-relaxed text-error">{problem}</p>}
      {selected && <p className="break-all text-[11px] leading-relaxed text-muted">{t("sites.detail.certDomains")} {selected.sans.join("、")}</p>}
      {query.isSuccess && !options.length && <p className="text-[11px] leading-relaxed text-muted">{t("sites.detail.certEmpty")}</p>}
      <p id={`${id}-hint`} className="text-[11px] leading-relaxed text-muted">{t(value === "local" ? "sites.wizard.httpsHint" : value.startsWith("acme:") ? "sites.detail.acmeHint" : "sites.detail.certSourceHint")}</p>
    </div>
  );
}
