"use client";

import * as React from "react";
import { ArrowUp, Bot, Check, ChevronDown, Loader2, MessageSquare, Plus, Settings2, ShieldCheck, Square, Wrench, X } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import * as api from "@/lib/api";
import { isTauri, normalizeError } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { useUI } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";

type Entry = { id: number; kind: "user" | "assistant" | "tool"; text: string; detail?: string; failed?: boolean };
const toolNames: Record<string, [string, string]> = {
  list_services: ["读取服务状态", "Read services"], list_packages: ["读取已安装套件", "Read packages"],
  list_sites: ["读取站点列表", "Read sites"], read_logs: ["读取最近日志", "Read recent logs"],
  diagnose_port: ["检查端口占用", "Check port"], diagnose_service: ["诊断服务", "Diagnose service"],
  repair_nginx: ["修复 Nginx upstream", "Repair Nginx upstream"], start_service: ["启动服务", "Start service"],
  stop_service: ["停止服务", "Stop service"], restart_service: ["重启服务", "Restart service"],
};
const storageKey = "niceenv-assistant-connection";
function failureText(error: unknown): string {
  const value = normalizeError(error);
  return [value.message, value.hint, value.detail].filter(Boolean).join("\n");
}
function savedConnection(): api.AssistantConnection {
  try {
    const value = JSON.parse(localStorage.getItem(storageKey) ?? "{}") as Partial<api.AssistantConnection>;
    return { endpoint: typeof value.endpoint === "string" ? value.endpoint : "https://api.openai.com/v1",
      model: typeof value.model === "string" ? value.model : "", apiKey: "" };
  } catch { return { endpoint: "https://api.openai.com/v1", model: "", apiKey: "" }; }
}

export function AssistantPanel({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const english = useUI((s) => s.lang) === "en";
  const say = (zh: string, en: string) => english ? en : zh;
  const qc = useQueryClient();
  const [connection, setConnection] = React.useState(savedConnection);
  const [settings, setSettings] = React.useState(() => !savedConnection().model);
  const [configured, setConfigured] = React.useState(false);
  const [models, setModels] = React.useState<string[]>([]);
  const [loadingModels, setLoadingModels] = React.useState(false);
  const [draft, setDraft] = React.useState("");
  const [entries, setEntries] = React.useState<Entry[]>([]);
  const [busy, setBusy] = React.useState(false);
  const [status, setStatus] = React.useState("");
  const [error, setError] = React.useState("");
  const [approval, setApproval] = React.useState<api.AssistantApproval | null>(null);
  const [executing, setExecuting] = React.useState(false);
  const [retry, setRetry] = React.useState(false);
  const history = React.useRef<api.AssistantMessage[]>([]);
  const sequence = React.useRef(0);
  const running = React.useRef(false);
  const confirming = React.useRef(false);
  const stopping = React.useRef(false);
  const decision = React.useRef<((reply: api.AssistantToolReply) => void) | null>(null);
  const end = React.useRef<HTMLDivElement>(null);
  const composer = React.useRef<HTMLTextAreaElement>(null);
  const append = (entry: Omit<Entry, "id">) => setEntries((rows) => [...rows, { ...entry, id: ++sequence.current }]);

  function updateConnection(next: api.AssistantConnection) {
    setConnection(next); setConfigured(false);
    if (next.endpoint !== connection.endpoint || next.apiKey !== connection.apiKey) setModels([]);
  }

  async function fetchModels() {
    setLoadingModels(true); setError("");
    try { setModels(await api.assistantModels(connection)); }
    catch (e) { setError(failureText(e)); }
    finally { setLoadingModels(false); }
  }

  React.useEffect(() => { end.current?.scrollIntoView({ block: "nearest" }); }, [entries.length, status, approval, open]);

  function saveConnection(event: React.FormEvent) {
    event.preventDefault();
    try {
      const url = new URL(connection.endpoint.trim());
      const local = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
      if ((url.protocol !== "https:" && !(local && url.protocol === "http:")) || url.username || url.password || url.search || url.hash || !connection.model.trim()) {
        throw new Error(say("请填写 HTTPS 服务地址和模型名称。本机服务可使用 HTTP；地址不能包含密钥或查询参数。", "Enter an HTTPS base URL and model. Localhost may use HTTP. Do not put credentials in the URL."));
      }
      // Never persist the key or conversation.
      localStorage.setItem(storageKey, JSON.stringify({ endpoint: url.href, model: connection.model.trim() }));
      setConnection((value) => ({ ...value, endpoint: url.href, model: value.model.trim() }));
      setConfigured(true); setSettings(false); setError("");
    } catch (e) { setError(e instanceof Error ? e.message : say("无法保存连接设置", "Could not save connection")); }
  }

  async function decide(approved: boolean) {
    if (!approval || confirming.current) return;
    confirming.current = true;
    setExecuting(true);
    let reply: api.AssistantToolReply;
    try {
      reply = await api.assistantConfirm(approval.token, approved);
      if (approved) { void qc.invalidateQueries({ queryKey: ["services"] }); void qc.invalidateQueries({ queryKey: ["configs"] }); }
    } catch (e) { reply = { result: JSON.stringify({ ok: false, error: failureText(e) }), approval: null }; }
    setApproval(null); setExecuting(false); confirming.current = false;
    decision.current?.(reply); decision.current = null;
  }

  async function run(text?: string) {
    if (running.current || !configured || !isTauri) return;
    if (history.current.length > 48) { setError(say("对话已较长，请新建对话后继续。", "Start a new conversation to continue.")); return; }
    running.current = true; stopping.current = false; setBusy(true); setError(""); setRetry(false);
    if (text) { history.current.push({ role: "user", content: text }); append({ kind: "user", text }); setDraft(""); }
    try {
      for (let round = 0; round < 4; round++) {
        setStatus(say("正在分析，请稍候…", "Analyzing…"));
        const message = await api.assistantChat(connection, history.current);
        if (stopping.current) { append({ kind: "assistant", text: say("已停止本轮，没有继续操作。", "Stopped this turn. No further actions were performed.") }); return; }
        history.current.push(message);
        if (message.content) append({ kind: "assistant", text: message.content });
        const calls = message.tool_calls ?? [];
        if (!calls.length) return;
        for (const call of calls) {
          const label = toolNames[call.function.name]?.[english ? 1 : 0] ?? call.function.name;
          setStatus(label);
          let reply: api.AssistantToolReply;
          try {
            reply = stopping.current ? { result: '{"cancelled":true}', approval: null }
              : await api.assistantTool(call.function.name, JSON.parse(call.function.arguments));
            if (reply.approval) {
              if (stopping.current) reply = await api.assistantConfirm(reply.approval.token, false);
              else {
                const pending = reply.approval;
                setStatus(say("等待你确认操作", "Waiting for your review"));
                reply = await new Promise<api.AssistantToolReply>((resolve) => { decision.current = resolve; setApproval(pending); });
              }
            }
          } catch (e) { reply = { result: JSON.stringify({ ok: false, error: failureText(e) }), approval: null }; }
          const content = reply.result ?? '{"ok":false,"error":"No result returned"}';
          history.current.push({ role: "tool", tool_call_id: call.id, content });
          let failed = false;
          let cancelled = false;
          let detail = content;
          try { const value = JSON.parse(content) as { ok?: boolean; error?: unknown; cancelled?: boolean }; failed = value.ok === false || !!value.error; cancelled = !!value.cancelled; detail = JSON.stringify(value, null, 2); } catch { /* Truncated results remain readable. */ }
          append({ kind: "tool", text: label + (cancelled ? say(" · 已取消", " · Cancelled") : ""), detail, failed });
        }
        if (stopping.current) { append({ kind: "assistant", text: say("已停止后续分析。已完成的操作见上方记录。", "Stopped. Completed actions are shown above.") }); return; }
      }
      append({ kind: "assistant", text: say("本轮已达到 4 次分析上限。你可以继续提问，我会基于已有结果分析。", "This turn reached its four-step analysis limit. Ask a follow-up to continue.") });
    } catch (e) { setError(failureText(e)); setRetry(true); }
    finally { running.current = false; setBusy(false); setStatus(""); composer.current?.focus(); }
  }

  function newChat() {
    history.current = []; setEntries([]); setError(""); setRetry(false);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent hideClose className="bottom-4 left-auto right-4 top-auto flex h-[min(720px,calc(100dvh-2rem))] w-[calc(100vw-2rem)] max-w-[460px] translate-x-0 translate-y-0 flex-col gap-0 overflow-hidden rounded-2xl border border-border bg-card p-0 shadow-2xl">
        <header className="flex items-center gap-3 border-b border-border px-5 py-4">
          <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-primary-soft text-primary"><Bot className="h-5 w-5" /></div>
          <div className="min-w-0 flex-1">
            <DialogTitle className="text-sm">{say("NiceEnv 环境助手", "NiceEnv Assistant")}</DialogTitle>
            <DialogDescription className="mt-1 text-[11px]">{say("诊断问题 · 解释原因 · 协助修复", "Diagnose · Understand · Repair")}</DialogDescription>
          </div>
          <Button variant="ghost" size="icon" disabled={busy || entries.length === 0} onClick={newChat} aria-label={say("新建对话", "New conversation")} title={say("新建对话", "New conversation")}><Plus className="h-4 w-4" /></Button>
          <Button variant="ghost" size="icon" disabled={busy || loadingModels} onClick={() => { setSettings(!settings); setError(""); }} aria-label={say("模型连接设置", "Connection settings")} title={say("模型连接设置", "Connection settings")}><Settings2 className="h-4 w-4" /></Button>
          <Button variant="ghost" size="icon" onClick={() => onOpenChange(false)} aria-label={say("收起助手", "Minimize assistant")}><X className="h-4 w-4" /></Button>
        </header>
        {settings ? (
          <form onSubmit={saveConnection} className="flex min-h-0 flex-1 flex-col gap-5 overflow-auto p-5">
            <div><h3 className="font-medium">{say("连接你的模型", "Connect your model")}</h3><p className="mt-1 text-xs leading-5 text-muted">{say("支持 OpenAI 兼容接口和本地模型。模型需要支持工具调用。", "Use an OpenAI-compatible or local provider with function calling support.")}</p></div>
            <div className="flex flex-wrap gap-2">
              <Button type="button" variant="outline" size="sm" disabled={loadingModels} onClick={() => updateConnection({ endpoint: "https://api.openai.com/v1", model: "", apiKey: "" })}>OpenAI</Button>
              <Button type="button" variant="outline" size="sm" disabled={loadingModels} onClick={() => updateConnection({ endpoint: "http://localhost:11434/v1", model: "", apiKey: "" })}>{say("本地 Ollama", "Local Ollama")}</Button>
            </div>
            <label className="space-y-2 text-xs">{say("服务地址", "Base URL")}<Input required disabled={loadingModels} type="url" value={connection.endpoint} maxLength={2048} onChange={(e) => updateConnection({ ...connection, endpoint: e.target.value, apiKey: "" })} placeholder="https://your-provider.example/v1" /></label>
            <label className="space-y-2 text-xs">API Key<Input type="password" disabled={loadingModels} autoComplete="off" value={connection.apiKey} maxLength={4096} onChange={(e) => updateConnection({ ...connection, apiKey: e.target.value })} placeholder={say("本地免密服务可留空", "Optional for local providers")} /></label>
            <label className="space-y-2 text-xs">{say("模型名称", "Model")}<div className="flex gap-2"><Input required list="assistant-models" disabled={loadingModels} value={connection.model} maxLength={200} onChange={(e) => updateConnection({ ...connection, model: e.target.value })} placeholder={say("获取列表后选择，或输入模型名称", "Fetch models to choose, or enter a model ID")} /><Button type="button" variant="outline" size="sm" disabled={loadingModels || !isTauri} onClick={() => void fetchModels()}>{loadingModels ? <Loader2 className="h-4 w-4 animate-spin" /> : say("获取模型", "Load models")}</Button></div><datalist id="assistant-models">{models.map((model) => <option key={model} value={model} />)}</datalist></label>
            <div className="rounded-xl bg-fill p-3 text-xs leading-5 text-muted"><ShieldCheck className="mb-2 h-4 w-4 text-primary" />{say("发送消息后，对话与工具查询结果会传给所选服务。常见凭据会隐藏，但路径、域名和日志仍可能包含业务信息。API Key 和对话仅留在本次应用会话；重启后需重新填写密钥。", "Messages and tool results go to your selected provider. Common credentials are redacted, but paths, domains and logs may contain business information. The API key and conversation remain in this app session only.")}</div>
            {!isTauri && <p className="text-xs text-muted">{say("浏览器预览无法连接模型或操作本机，请在桌面应用使用。", "Use the desktop app to connect a model and operate local services.")}</p>}
            {error && <p role="alert" className="text-xs text-error">{error}</p>}
            <Button type="submit" disabled={!isTauri || loadingModels} className="mt-auto">{say("保存连接并开始", "Save connection & start")}</Button>
          </form>
        ) : (
          <>
            <div className="flex items-center justify-between gap-2 border-b border-border bg-fill/50 px-5 py-2 text-[11px] text-muted">
              <span className="truncate">{connection.model || say("尚未连接", "Not connected")}</span>
              <span className="shrink-0">{say("操作前逐项确认", "Review each action")}</span>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 py-5">
              {!entries.length && <div className="flex flex-col items-start gap-4 py-5">
                <MessageSquare className="h-7 w-7 text-primary" />
                <h3 className="text-xl font-semibold tracking-tight">{say("从一个问题开始。", "Start with a question.")}</h3>
                <p className="text-sm leading-6 text-muted">{say("我可以检查服务、查看最近日志，或协助修复 Nginx 的 PHP upstream。先看原因，再决定怎么处理。", "Inspect services, review recent logs, or repair missing PHP upstreams in Nginx. Understand the cause before making changes.")}</p>
                {["帮我检查 Nginx 为什么启动失败", "检查有哪些服务没有正常运行", "解释 Windows 路径里的 \\\\?\\ 前缀"].map((text, i) => {
                  const prompt = english ? ["Why does Nginx fail to start?", "Check which services are unhealthy", "Explain the Windows extended path prefix"][i] : text;
                  return <button key={text} disabled={busy || !configured} onClick={() => void run(prompt)} className="flex w-full items-center justify-between gap-3 rounded-xl border border-border px-3 py-3 text-left text-xs transition-colors hover:bg-fill focus-visible:outline focus-visible:outline-primary disabled:opacity-50">{prompt}<ArrowUp className="h-3.5 w-3.5 rotate-45" /></button>;
                })}
                {!configured && <Button onClick={() => setSettings(true)}>{say("配置模型连接", "Configure connection")}</Button>}
              </div>}
              <div className="space-y-4">
                {entries.map((entry) => entry.kind === "tool" ? (
                  <details key={entry.id} className={cn("rounded-xl border border-border bg-fill/50 text-xs", entry.failed && "border-error/30")}>
                    <summary className="flex cursor-pointer list-none items-center gap-2 p-3">{entry.failed ? <Wrench className="h-3.5 w-3.5 text-error" /> : <Check className="h-3.5 w-3.5 text-primary" />}<span className="flex-1">{entry.text}{entry.failed ? say(" · 未完成", " · Failed") : ""}</span><ChevronDown className="h-3.5 w-3.5 text-faint" /></summary>
                    <pre className="max-h-52 overflow-auto whitespace-pre-wrap break-words border-t border-border p-3 text-[11px] text-muted [overflow-wrap:anywhere]">{entry.detail}</pre>
                  </details>
                ) : <div key={entry.id} className={cn("whitespace-pre-wrap break-words text-[13px] leading-6 [overflow-wrap:anywhere]", entry.kind === "user" ? "ml-8 rounded-2xl rounded-br-sm bg-primary-soft px-4 py-3 text-foreground" : "pr-2")}>{entry.text}</div>)}
              </div>
              {busy && <div role="status" className="mt-4 flex items-center gap-2 text-xs text-muted"><Loader2 className="h-3.5 w-3.5 animate-spin" />{status}</div>}
              {approval && <div className="mt-4 rounded-xl border border-primary/30 bg-primary-soft/30 p-4">
                <h4 className="text-sm font-semibold">{approval.title}</h4>
                <pre className="mt-2 max-h-44 overflow-auto whitespace-pre-wrap break-words font-sans text-xs leading-5 text-muted [overflow-wrap:anywhere]">{approval.detail}</pre>
                <p className="mt-2 text-[11px] text-faint">{say("10 分钟内有效；只执行这一次操作。", "Valid for 10 minutes. Applies to this action only.")}</p>
                <div className="mt-3 flex gap-2"><Button size="sm" disabled={executing} onClick={() => void decide(true)}>{executing && <Loader2 className="mr-1 h-3 w-3 animate-spin" />}{say("执行此操作", "Execute action")}</Button><Button size="sm" variant="outline" disabled={executing} onClick={() => void decide(false)}>{say("取消", "Cancel")}</Button></div>
              </div>}
              {error && <div role="alert" className="mt-4 rounded-xl bg-error-soft p-3 text-xs text-error">{error}{retry && <Button size="sm" variant="ghost" className="mt-2" disabled={busy} onClick={() => void run()}>{say("重试分析", "Retry analysis")}</Button>}</div>}
              <div ref={end} />
            </div>
            <form onSubmit={(event) => { event.preventDefault(); if (draft.trim()) void run(draft.trim()); }} className="border-t border-border p-4">
              <div className="rounded-xl border border-border bg-fill p-2 focus-within:border-primary">
                <textarea ref={composer} value={draft} onChange={(e) => setDraft(e.target.value)} rows={2} maxLength={8000} disabled={busy || !configured || !isTauri}
                  onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); if (draft.trim()) void run(draft.trim()); } }}
                  aria-label={say("发送给环境助手的消息", "Message the assistant")} placeholder={say("描述问题，或粘贴报错…", "Describe a problem or paste an error…")}
                  className="w-full resize-none bg-transparent p-1 text-sm outline-none placeholder:text-faint disabled:opacity-60" />
                <div className="flex items-center justify-between gap-2"><span className="pl-1 text-[10px] text-faint">Enter {say("发送", "send")} · Shift+Enter {say("换行", "newline")}</span>
                  {busy && !approval ? <Button type="button" variant="ghost" size="sm" onClick={() => { stopping.current = true; setStatus(say("正在停止，等待当前请求结束…", "Stopping after the current request…")); }} aria-label={say("停止后续分析", "Stop analysis")}><Square className="h-3.5 w-3.5" /></Button>
                    : <Button type="submit" size="icon" disabled={busy || !configured || !draft.trim() || !isTauri} aria-label={say("发送消息", "Send message")}><ArrowUp className="h-4 w-4" /></Button>}
                </div>
              </div>
              <p className="mt-2 text-center text-[10px] text-faint">{say("AI 建议可能有误，请结合检查结果判断。", "AI may make mistakes. Review the diagnostic evidence.")}</p>
            </form>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
