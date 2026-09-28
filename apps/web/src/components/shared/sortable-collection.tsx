"use client";

import * as React from "react";
import { createPortal } from "react-dom";
import {
  DndContext, DragOverlay, KeyboardSensor, PointerSensor, closestCenter,
  defaultDropAnimationSideEffects, useSensor, useSensors,
} from "@dnd-kit/core";
import {
  SortableContext, rectSortingStrategy, verticalListSortingStrategy,
  sortableKeyboardCoordinates, useSortable,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { useReducedMotion } from "motion/react";
import { GripVertical, RotateCcw } from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { useSettings } from "@/lib/hooks";
import { cn, orderedDisplayIds, reorderVisibleIds } from "@/lib/utils";

const EMPTY_ORDER: string[] = [];
const EASING = "cubic-bezier(0.22, 1, 0.36, 1)";

type CollectionProps<T extends { id: string }> = {
  items: T[];
  allIds?: string[];
  scope: "packages" | "services";
  label: (item: T) => string;
  children: (item: T, handle: React.ReactNode, preview?: boolean) => React.ReactNode;
  className: string;
  grid?: boolean;
  disabled?: boolean;
};

/** 独立把手避免启停/菜单误触；轮询保留业务组件，落下时才持久化显示顺序。 */
export function SortableCollection<T extends { id: string }>(props: CollectionProps<T>) {
  // 筛选、卸载或视图变化时取消正在进行的拖动，不保存过期的目标位置。
  const membership = JSON.stringify(props.items.map((item) => item.id).sort());
  return <CollectionSession key={`${props.scope}:${!!props.grid}:${!!props.disabled}:${membership}`} {...props} />;
}

function CollectionSession<T extends { id: string }>({ items, allIds, scope, label, children, className, grid, disabled }: CollectionProps<T>) {
  const t = useT();
  const saved = useUI((s) => s.displayOrder?.[scope] ?? EMPTY_ORDER);
  const setOrder = useUI((s) => s.setDisplayOrder);
  const { data: settings } = useSettings();
  const systemReduced = useReducedMotion();
  const reduced = !!settings?.reduceMotion || !!systemReduced;
  const [activeId, setActiveId] = React.useState<string | null>(null);
  const listRef = React.useRef<HTMLDivElement>(null);
  const contextId = React.useId();
  const fullOrder = orderedDisplayIds(allIds ?? items.map((item) => item.id), saved);
  const byId = new Map(items.map((item) => [item.id, item]));
  const visibleIds = fullOrder.filter((id) => byId.has(id));
  const activeItem = activeId ? byId.get(activeId) : undefined;
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  const nameOf = (id: string | number) => {
    const item = byId.get(String(id));
    return item ? label(item) : String(id);
  };
  const announcePosition = (id: string | number, target: string | number) => t("sort.position")
    .replace("{name}", nameOf(id)).replace("{position}", String(visibleIds.indexOf(String(target)) + 1))
    .replace("{total}", String(visibleIds.length));

  if (!items.length) return null;
  return (
    <div className="space-y-2">
      {items.length > 1 && <div className="flex flex-wrap items-center justify-between gap-2 text-[11px] text-faint">
        <span>{t("sort.hint")}</span>
        <button type="button" disabled={disabled || activeId !== null || saved.length === 0}
          onClick={() => setOrder(scope, [])}
          className="inline-flex items-center gap-1 rounded-md px-2 py-1 hover:bg-fill hover:text-foreground focus-visible:outline-2 focus-visible:outline-primary disabled:opacity-40">
          <RotateCcw className="h-3 w-3" />{t("sort.reset")}
        </button>
      </div>}
      <DndContext id={contextId} sensors={sensors} collisionDetection={(args) => {
        const bounds = listRef.current?.getBoundingClientRect();
        const point = args.pointerCoordinates;
        if (bounds && point && (point.x < bounds.left || point.x > bounds.right || point.y < bounds.top || point.y > bounds.bottom)) return [];
        return closestCenter(args);
      }}
        accessibility={{
          screenReaderInstructions: { draggable: t("sort.keyboard") },
          announcements: {
            onDragStart: ({ active }) => `${nameOf(active.id)}。${t("sort.keyboard")}`,
            onDragOver: ({ active, over }) => over ? announcePosition(active.id, over.id) : t("sort.outside"),
            onDragEnd: ({ active, over }) => over ? `${announcePosition(active.id, over.id)}。${t("sort.saved")}` : t("sort.cancelled"),
            onDragCancel: () => t("sort.cancelled"),
          },
        }}
        onDragStart={({ active }) => setActiveId(String(active.id))}
        onDragCancel={() => setActiveId(null)}
        onDragEnd={({ active, over }) => {
          setActiveId(null);
          if (!disabled && over && active.id !== over.id) {
            setOrder(scope, reorderVisibleIds(fullOrder, visibleIds, String(active.id), String(over.id)));
          }
        }}>
        <SortableContext items={visibleIds} strategy={grid ? rectSortingStrategy : verticalListSortingStrategy}>
          <div ref={listRef} className={className}>
            {visibleIds.map((id) => {
              const item = byId.get(id)!;
              return <SortableEntry key={id} id={id} label={label(item)} disabled={disabled || items.length < 2} reduced={reduced}>
                {(handle) => children(item, handle)}
              </SortableEntry>;
            })}
          </div>
        </SortableContext>
        {typeof document !== "undefined" && createPortal(
          <DragOverlay zIndex={80} dropAnimation={reduced ? null : {
            duration: 240, easing: EASING,
            sideEffects: defaultDropAnimationSideEffects({ styles: { active: { opacity: "0" } } }),
          }}>
            {activeItem ? <div inert aria-hidden="true" className="pointer-events-none rounded-xl bg-card shadow-[var(--shadow-pop)] ring-2 ring-primary/35 [&_*]:!animate-none [&_*]:!transition-none">
              {children(activeItem, <span className="flex h-8 w-6 shrink-0 items-center justify-center text-primary"><GripVertical className="h-4 w-4" /></span>, true)}
            </div> : null}
          </DragOverlay>, document.body)}
      </DndContext>
    </div>
  );
}

function SortableEntry({ id, label, disabled, reduced, children }: {
  id: string; label: string; disabled?: boolean; reduced: boolean;
  children: (handle: React.ReactNode) => React.ReactNode;
}) {
  const t = useT();
  const { attributes, listeners, setNodeRef, setActivatorNodeRef, transform, transition, isDragging } = useSortable({
    id, disabled, transition: { duration: reduced ? 0 : 240, easing: EASING },
  });
  return <div ref={setNodeRef} style={{ transform: CSS.Transform.toString(transform), transition }}
    className={cn("relative min-w-0", isDragging && "rounded-xl bg-primary-soft outline-2 outline-dashed outline-primary/35 [&>*]:opacity-20")}>
    {children(<button type="button" ref={setActivatorNodeRef} {...attributes} {...listeners} disabled={disabled}
      aria-label={t("sort.handle").replace("{name}", label)} title={t("sort.keyboard")}
      className="nsb-no-drag flex h-8 w-6 shrink-0 touch-none select-none items-center justify-center rounded-md text-faint transition-colors hover:bg-fill hover:text-primary focus-visible:outline-2 focus-visible:outline-primary enabled:cursor-grab enabled:active:cursor-grabbing disabled:opacity-25">
      <GripVertical className="h-4 w-4" />
    </button>)}
  </div>;
}
