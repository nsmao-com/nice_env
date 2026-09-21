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
          toast: "glass-toast group toast group-[.toaster]:text-foreground group-[.toaster]:rounded-2xl",
          description: "group-[.toast]:text-muted",
          actionButton: "group-[.toast]:bg-primary group-[.toast]:text-primary-fg",
          cancelButton: "group-[.toast]:bg-fill group-[.toast]:text-muted",
        },
      }}
      {...props}
    />
  );
};

export { Toaster };
