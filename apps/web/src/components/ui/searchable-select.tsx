"use client";

import * as React from "react";
import { Check, ChevronDown, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { Popover, PopoverContent, PopoverTrigger } from "./popover";
import { Command, CommandGroup, CommandInput, CommandItem, CommandList, CommandSeparator } from "./command";

export type SearchableOption = {
  value: string;
  label: React.ReactNode;
  searchText: string;
  group?: string;
  /** The original Select contained a separator immediately before this option. */
  separatorBefore?: boolean;
  disabled?: boolean;
  className?: string;
  style?: React.CSSProperties;
};

/** Combobox for long Select lists. Filtering never changes the committed value. */
export function SearchableSelect({ options, value, onValueChange, open, onOpenChange, disabled,
  placeholder, searchPlaceholder, triggerProps, contentClassName, dir,
}: {
  options: SearchableOption[];
  value?: string;
  onValueChange: (value: string) => void;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  disabled?: boolean;
  placeholder?: React.ReactNode;
  searchPlaceholder?: string;
  triggerProps: React.ButtonHTMLAttributes<HTMLButtonElement>;
  contentClassName?: string;
  dir?: "ltr" | "rtl";
}) {
  const t = useT();
  const input = React.useRef<HTMLInputElement>(null);
  const trigger = React.useRef<HTMLButtonElement>(null);
  const [query, setQuery] = React.useState("");
  const selected = options.find(item => item.value === value);
  const normalize = (text: string) => text.normalize("NFKC").toLocaleLowerCase();
  const words = normalize(query).trim().split(/\s+/).filter(Boolean);
  const visible = options.filter(item => words.every(word => normalize(item.searchText).includes(word)));
  const groups = [...new Set(visible.map(item => item.group ?? ""))];
  const changeOpen = (next: boolean) => {
    setQuery("");
    onOpenChange(next);
  };
  // A parent can close or disable this field while an async action is in progress.
  React.useEffect(() => { if (!open) setQuery(""); }, [open]);
  React.useEffect(() => { if (disabled && open) onOpenChange(false); }, [disabled, open, onOpenChange]);
  const { className, onKeyDown, children: _children, ...buttonProps } = triggerProps;
  return <Popover open={open && !disabled} onOpenChange={changeOpen}>
    <PopoverTrigger asChild>
      <button {...buttonProps} ref={trigger} type="button" disabled={disabled || triggerProps.disabled}
        dir={dir} aria-haspopup="dialog" aria-expanded={open && !disabled} data-placeholder={!value ? "" : undefined}
        className={cn("flex h-9 w-full items-center justify-between gap-2 rounded-md border border-border bg-fill px-3 py-2 text-left text-sm focus:outline-none focus-visible:border-primary disabled:cursor-not-allowed disabled:opacity-50", className)}
        onKeyDown={event => {
          onKeyDown?.(event);
          if (!event.defaultPrevented && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
            event.preventDefault(); changeOpen(true);
          }
        }}>
        <span className="min-w-0 flex-1 truncate">{selected?.label ?? (value || placeholder)}</span>
        <ChevronDown className="h-4 w-4 shrink-0 opacity-50" />
      </button>
    </PopoverTrigger>
    <PopoverContent align="start" collisionPadding={12} dir={dir}
      className={cn("flex w-[var(--radix-popover-trigger-width)] min-w-[min(20rem,calc(100vw-24px))] max-w-[calc(100vw-24px)] max-h-[var(--radix-popover-content-available-height)] flex-col overflow-hidden p-0", contentClassName)}
      aria-label={searchPlaceholder ?? t("select.search")}
      onOpenAutoFocus={event => { event.preventDefault(); input.current?.focus(); }}>
      <Command shouldFilter={false} defaultValue={selected?.disabled ? undefined : value} loop className="min-h-0 rounded-none"
        onKeyDownCapture={event => {
          // IME confirmation must not also commit the highlighted option.
          if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) event.stopPropagation();
        }}>
        <div className="relative shrink-0">
          <CommandInput ref={input} value={query} onValueChange={setQuery}
            aria-label={searchPlaceholder ?? t("select.search")} placeholder={searchPlaceholder ?? t("select.search")}
            className="pr-8" />
          {query && <button type="button" aria-label={t("select.clearSearch")}
            className="absolute right-3 top-2 flex h-8 w-8 items-center justify-center rounded-full text-muted hover:bg-fill focus-visible:outline-primary"
            onClick={() => { setQuery(""); input.current?.focus(); }}><X className="h-3.5 w-3.5" /></button>}
        </div>
        <CommandList className="max-h-72 flex-1">
          {!visible.length && <div role="status" className="px-4 py-8 text-center text-sm text-muted">{t(query.trim() ? "select.noResults" : "select.empty")}</div>}
          {groups.map(group => <CommandGroup key={group} heading={group || undefined}>
            {visible.filter(item => (item.group ?? "") === group).map(item => <React.Fragment key={item.value}>
              {item.separatorBefore && <CommandSeparator />}
              <CommandItem value={item.value}
                disabled={item.disabled} className={cn("min-h-9 gap-2 py-2", item.className)} style={item.style}
                onSelect={() => { if (!item.disabled) { onValueChange(item.value); changeOpen(false); } }}>
                <span className="min-w-0 flex-1 break-words">{item.label}</span>
                {item.value === value && <Check aria-label={t("select.selected")} className="h-4 w-4 shrink-0 text-primary" />}
              </CommandItem>
            </React.Fragment>)}
          </CommandGroup>)}
        </CommandList>
        <div role="status" className="mx-2 shrink-0 border-t border-dashed border-separator px-2 py-2 text-xs text-muted">
          {t("select.count").replace("{visible}", String(visible.length)).replace("{total}", String(options.length))}
        </div>
      </Command>
    </PopoverContent>
  </Popover>;
}
