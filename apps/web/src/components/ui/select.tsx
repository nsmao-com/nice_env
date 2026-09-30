"use client";

import * as React from "react";
import * as SelectPrimitive from "@radix-ui/react-select";
import { Check, ChevronDown, ChevronUp } from "lucide-react";
import { cn } from "@/lib/utils";
import { SearchableSelect, type SearchableOption } from "./searchable-select";

const SelectGroup = SelectPrimitive.Group;
const SelectValue = SelectPrimitive.Value;

const SelectTrigger = React.forwardRef<
  React.ComponentRef<typeof SelectPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Trigger>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Trigger
    ref={ref}
    className={cn(
      "flex h-9 w-full items-center justify-between gap-2 whitespace-nowrap rounded-md border border-border bg-fill px-3 py-2 text-sm placeholder:text-faint focus:outline-none focus:border-primary disabled:cursor-not-allowed disabled:opacity-50 [&>span]:line-clamp-1 cursor-pointer",
      className
    )}
    {...props}
  >
    {children}
    <SelectPrimitive.Icon asChild>
      <ChevronDown className="h-4 w-4 opacity-50" />
    </SelectPrimitive.Icon>
  </SelectPrimitive.Trigger>
));
SelectTrigger.displayName = "SelectTrigger";

const SelectContent = React.forwardRef<
  React.ComponentRef<typeof SelectPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Content>
>(({ className, children, position = "popper", ...props }, ref) => (
  <SelectPrimitive.Portal>
    <SelectPrimitive.Content
      ref={ref}
      className={cn(
        "glass-pop relative z-50 max-h-[min(24rem,var(--radix-select-content-available-height))] min-w-[8rem] max-w-[calc(100vw-24px)] overflow-hidden rounded-2xl text-foreground data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
        position === "popper" && "data-[side=bottom]:translate-y-1 data-[side=top]:-translate-y-1",
        className
      )}
      position={position}
      {...props}
    >
      <SelectPrimitive.ScrollUpButton className="flex items-center justify-center py-1">
        <ChevronUp className="h-4 w-4" />
      </SelectPrimitive.ScrollUpButton>
      <SelectPrimitive.Viewport
        className={cn(
          "p-1",
          position === "popper" &&
            "w-full min-w-[var(--radix-select-trigger-width)]"
        )}
      >
        {children}
      </SelectPrimitive.Viewport>
      <SelectPrimitive.ScrollDownButton className="flex items-center justify-center py-1">
        <ChevronDown className="h-4 w-4" />
      </SelectPrimitive.ScrollDownButton>
    </SelectPrimitive.Content>
  </SelectPrimitive.Portal>
));
SelectContent.displayName = "SelectContent";

const SelectLabel = React.forwardRef<
  React.ComponentRef<typeof SelectPrimitive.Label>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Label>
>(({ className, ...props }, ref) => (
  <SelectPrimitive.Label
    ref={ref}
    className={cn("px-2 py-1.5 text-xs text-faint", className)}
    {...props}
  />
));
SelectLabel.displayName = "SelectLabel";

const SelectItem = React.forwardRef<
  React.ComponentRef<typeof SelectPrimitive.Item>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Item>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Item
    ref={ref}
    className={cn(
      "relative flex w-full cursor-pointer select-none items-center rounded-lg py-1.5 pl-2 pr-8 text-sm outline-none focus:bg-fill focus:text-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
      className
    )}
    {...props}
  >
    <span className="absolute right-2 flex h-3.5 w-3.5 items-center justify-center">
      <SelectPrimitive.ItemIndicator>
        <Check className="h-4 w-4 text-primary" />
      </SelectPrimitive.ItemIndicator>
    </span>
    <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
  </SelectPrimitive.Item>
));
SelectItem.displayName = "SelectItem";

const SelectSeparator = React.forwardRef<
  React.ComponentRef<typeof SelectPrimitive.Separator>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Separator>
>(({ className, ...props }, ref) => (
  // 虚线 + 左右留边：通栏实线太重，内缩的细虚线更接近 Apple 分组分隔线
  <SelectPrimitive.Separator
    ref={ref}
    className={cn("mx-3 my-1 h-0 border-0 border-t border-dashed border-separator", className)}
    {...props}
  />
));
SelectSeparator.displayName = "SelectSeparator";

function textOf(node: React.ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join(" ");
  return React.isValidElement<{ children?: React.ReactNode }>(node) ? textOf(node.props.children) : "";
}

/** Read the existing declarative options without changing their labels or stored values. */
function searchableOptions(children: React.ReactNode, group = "", leadingSeparator = false): SearchableOption[] | null {
  const options: SearchableOption[] = [];
  let supported = true;
  let separatorBefore = leadingSeparator;
  React.Children.forEach(children, child => {
    if (child == null || typeof child === "boolean") return;
    if (!React.isValidElement<{ children?: React.ReactNode }>(child)) { supported = false; return; }
    if (child.type === SelectItem) {
      const props = (child as React.ReactElement<React.ComponentProps<typeof SelectItem>>).props;
      const searchText = props.textValue ?? textOf(props.children);
      if (!searchText) { supported = false; return; }
      options.push({ value: props.value, label: props.children, searchText, group, separatorBefore, disabled: props.disabled, className: props.className, style: props.style });
      separatorBefore = false;
    } else if (child.type === SelectGroup || child.type === React.Fragment) {
      const label = React.Children.toArray(child.props.children).find(item => React.isValidElement(item) && item.type === SelectLabel);
      const nested = searchableOptions(child.props.children, label ? textOf(label) : group, separatorBefore);
      if (nested) { options.push(...nested); separatorBefore = false; } else supported = false;
    } else if (child.type === SelectSeparator) {
      // Preserve explicit separators when the list is upgraded to SearchableSelect.
      separatorBefore = true;
    } else if (child.type !== SelectLabel) {
      // Custom option components keep Radix behavior unless they expose ordinary SelectItems.
      supported = false;
    }
  });
  return supported ? options : null;
}

function Select({ children, searchable, searchPlaceholder, ...props }: React.ComponentProps<typeof SelectPrimitive.Root> & {
  /** Long lists are searchable automatically; true also enables search for a short list. */
  searchable?: boolean;
  searchPlaceholder?: string;
}) {
  const [localValue, setLocalValue] = React.useState(props.defaultValue);
  const [localOpen, setLocalOpen] = React.useState(props.defaultOpen ?? false);
  const value = props.value !== undefined ? props.value : localValue;
  const open = props.open !== undefined ? props.open : localOpen;
  const onValueChange = (next: string) => { setLocalValue(next); props.onValueChange?.(next); };
  const onOpenChange = (next: boolean) => { setLocalOpen(next); props.onOpenChange?.(next); };
  const nodes = React.Children.toArray(children);
  const trigger = nodes.find(child => React.isValidElement(child) && child.type === SelectTrigger) as React.ReactElement<React.ComponentProps<typeof SelectTrigger>> | undefined;
  const content = nodes.find(child => React.isValidElement(child) && child.type === SelectContent) as React.ReactElement<React.ComponentProps<typeof SelectContent>> | undefined;
  const selection = trigger && React.Children.toArray(trigger.props.children);
  const display = selection?.length === 1 && React.isValidElement(selection[0]) && selection[0].type === SelectValue
    ? selection[0] as React.ReactElement<React.ComponentProps<typeof SelectValue>> : undefined;
  const options = content ? searchableOptions(content.props.children) : null;
  // Keep native form integration and custom trigger/content contracts on the Radix path.
  const supported = trigger && content && display && !display.props.children && !trigger.props.asChild && !trigger.props.ref
    && !content.props.asChild && !content.props.ref && !content.props.onCloseAutoFocus && !content.props.onEscapeKeyDown
    && Object.keys(content.props).every(key => ["children", "className", "position"].includes(key))
    && !props.name && !props.form && !props.required;
  if (supported && options && (searchable ?? options.length >= 8)) {
    return <SearchableSelect options={options} value={value} onValueChange={onValueChange} open={open}
      onOpenChange={onOpenChange} disabled={props.disabled || trigger.props.disabled} dir={props.dir}
      placeholder={display.props.placeholder} searchPlaceholder={searchPlaceholder}
      triggerProps={trigger.props} contentClassName={content.props.className} />;
  }
  return <SelectPrimitive.Root {...props} value={value} open={open} onValueChange={onValueChange} onOpenChange={onOpenChange}>{children}</SelectPrimitive.Root>;
}

export {
  Select,
  SelectGroup,
  SelectValue,
  SelectTrigger,
  SelectContent,
  SelectLabel,
  SelectItem,
  SelectSeparator,
};
