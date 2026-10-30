import * as React from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import { browserUrl } from "@/lib/backend";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";

function CodeBlock({ children, english }: { children: React.ReactNode; english: boolean }) {
  const ref = React.useRef<HTMLPreElement>(null);
  const [copied, setCopied] = React.useState(false);
  const [failed, setFailed] = React.useState(false);
  return <div className="my-3 overflow-hidden rounded-lg border border-border bg-fill">
    <div className="flex items-center justify-between border-b border-border px-3 py-1 text-[10px] text-faint">
      <span>{failed ? (english ? "Copy failed; select text to copy" : "复制失败，可选中文字复制") : (english ? "Code" : "代码")}</span>
      <Button size="sm" variant="ghost" className="h-7 gap-1 text-[11px]" onClick={() => {
        if (!navigator.clipboard) { setFailed(true); return; }
        void navigator.clipboard.writeText(ref.current?.textContent ?? "").then(() => { setCopied(true); setFailed(false); }, () => setFailed(true));
      }}>{copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}{copied ? (english ? "Copied" : "已复制") : (english ? "Copy" : "复制")}</Button>
    </div>
    <pre ref={ref} className="max-h-80 overflow-auto p-3 text-[12px] leading-5 [&>code]:bg-transparent [&>code]:p-0">{children}</pre>
  </div>;
}

export const AssistantMarkdown = React.memo(function AssistantMarkdown({ text, english }: { text: string; english: boolean }) {
  const [linkError, setLinkError] = React.useState(false);
  return <div className="min-w-0 space-y-2 break-words text-[13px] leading-6 [overflow-wrap:anywhere]">
    <Markdown remarkPlugins={[remarkGfm]} skipHtml urlTransform={(value) => {
      try { return browserUrl(value); } catch { return ""; }
    }} components={{
      h1: ({ children }) => <h3 className="mt-4 text-base font-semibold">{children}</h3>,
      h2: ({ children }) => <h3 className="mt-4 text-sm font-semibold">{children}</h3>,
      h3: ({ children }) => <h4 className="mt-3 font-semibold">{children}</h4>,
      p: ({ children }) => <p className="whitespace-pre-wrap">{children}</p>,
      ul: ({ children }) => <ul className="list-disc space-y-1 pl-5">{children}</ul>,
      ol: ({ children }) => <ol className="list-decimal space-y-1 pl-5">{children}</ol>,
      blockquote: ({ children }) => <blockquote className="border-l-2 border-primary/40 pl-3 text-muted">{children}</blockquote>,
      pre: ({ children }) => <CodeBlock english={english}>{children}</CodeBlock>,
      code: ({ children }) => <code className="rounded bg-fill px-1 py-0.5 font-mono text-[12px]">{children}</code>,
      table: ({ children }) => <div className="my-3 overflow-x-auto rounded-lg border border-border"><table className="w-full border-collapse text-xs">{children}</table></div>,
      th: ({ children }) => <th className="border-b border-border bg-fill px-3 py-2 text-left font-medium">{children}</th>,
      td: ({ children }) => <td className="border-b border-border px-3 py-2 align-top">{children}</td>,
      a: ({ href, children }) => href ? <a href={href} title={href} className="text-primary underline underline-offset-2" onClick={(event) => {
        event.preventDefault(); void api.openInBrowser(href).catch(() => setLinkError(true));
      }}>{children}</a> : <span>{children}</span>,
      // Do not automatically request model-supplied remote image URLs.
      img: ({ alt }) => <span className="text-muted">[{english ? "Image" : "图片"}: {alt || "—"}]</span>,
    }}>{text}</Markdown>
    {linkError && <p role="alert" className="text-xs text-error">{english ? "Could not open the link." : "无法打开链接，请检查地址。"}</p>}
  </div>;
});
