import { FolderOpen, Loader2 } from "lucide-react";
import { toast } from "sonner";
import { useQuery } from "@tanstack/react-query";
import type { DatabaseDataDirEngine } from "@/lib/api";
import * as api from "@/lib/api";
import { isTauri, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";

export function DatabaseDataDir({ engine, version, enabled = true }: {
  engine: DatabaseDataDirEngine;
  version?: string;
  enabled?: boolean;
}) {
  const t = useT();
  const query = useQuery({
    queryKey: ["database-data-dir", engine, version],
    queryFn: () => api.databaseDataDir(engine, version!),
    enabled: enabled && !!version,
    retry: false,
    refetchOnWindowFocus: false,
  });
  const open = async () => {
    if (!query.data) return;
    try {
      await api.openInFolder(query.data);
    } catch (error) {
      const parsed = normalizeError(error);
      toast.error(parsed.message, { description: parsed.hint });
    }
  };
  const unavailable = !version || query.isError;
  return (
    <div className="mt-3 min-w-0 rounded-md bg-card-2/50 px-2.5 py-2">
      <div className="flex min-w-0 flex-wrap items-center gap-2">
        <FolderOpen className="h-3.5 w-3.5 shrink-0 text-faint" />
        <span className="text-[11px] font-medium text-secondary">{t("db.datadir")}</span>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="ml-auto h-auto min-h-7 shrink-0 px-2 py-1 text-xs"
          disabled={!isTauri || !query.data || query.isFetching}
          title={!isTauri ? t("db.dataDirBrowserHint") : undefined}
          onClick={() => void open()}
        >
          {query.isFetching ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <FolderOpen className="h-3.5 w-3.5" />}
          {t("db.openDataDir")}
        </Button>
      </div>
      <code className={`mt-1 block min-w-0 break-all font-mono text-[11px] ${unavailable ? "text-faint" : "text-secondary"}`}>
        {version && query.isPending ? t("db.loading") : query.data ?? t("db.dataDirUnavailable")}
      </code>
      {query.isError && <p className="mt-1 break-words text-[11px] text-error">{t("db.dataDirReadFailed")}</p>}
      {!isTauri && <p className="mt-1 text-[11px] leading-4 text-muted">{t("db.dataDirBrowserHint")}</p>}
    </div>
  );
}
