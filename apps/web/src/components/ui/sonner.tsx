"use client";

import { useTheme } from "next-themes";
import { Toaster as Sonner } from "sonner";

type ToasterProps = React.ComponentProps<typeof Sonner>;

const Toaster = ({ ...props }: ToasterProps) => {
  const { theme = "dark" } = useTheme();

  return (
    <Sonner
      theme={theme as ToasterProps["theme"]}
      className="toaster group"
      position="bottom-right"
      offset={20}
      toastOptions={{
        classNames: {
          toast:
            "group toast relative sheen group-[.toaster]:bg-surface group-[.toaster]:text-foreground group-[.toaster]:border-border group-[.toaster]:shadow-xl group-[.toaster]:rounded-xl",
          description: "group-[.toast]:text-muted",
          actionButton: "group-[.toast]:bg-primary group-[.toast]:text-primary-fg",
          cancelButton: "group-[.toast]:bg-card-2 group-[.toast]:text-muted",
        },
      }}
      {...props}
    />
  );
};

export { Toaster };
