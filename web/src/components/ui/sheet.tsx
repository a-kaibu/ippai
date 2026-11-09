// A panel along the right edge (設定 and a game's information): radix's dialog without its overlay and
// without being modal, so the list beside it stays in use; Escape or × closes it.

import * as React from "react";
import { Dialog as SheetPrimitive } from "radix-ui";
import { XIcon } from "lucide-react";
import { cn } from "@/lib/utils";

function Sheet(props: React.ComponentProps<typeof SheetPrimitive.Root>) {
  return <SheetPrimitive.Root data-slot="sheet" modal={false} {...props} />;
}

function SheetContent({
  className,
  children,
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Content>) {
  return (
    <SheetPrimitive.Portal>
      <SheetPrimitive.Content
        data-slot="sheet-content"
        className={cn(
          "fixed inset-y-0 right-0 z-50 flex w-full flex-col border-l bg-background shadow-2xl outline-none sm:w-96",
          className,
        )}
        {...props}
      >
        {children}
        <SheetPrimitive.Close className="absolute top-3 right-3 grid size-8 place-items-center rounded-full bg-background/70 text-muted-foreground backdrop-blur transition-colors hover:bg-muted hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none">
          <XIcon className="size-4" />
          <span className="sr-only">閉じる</span>
        </SheetPrimitive.Close>
      </SheetPrimitive.Content>
    </SheetPrimitive.Portal>
  );
}

function SheetTitle({ className, ...props }: React.ComponentProps<typeof SheetPrimitive.Title>) {
  return (
    <SheetPrimitive.Title
      data-slot="sheet-title"
      className={cn("text-lg leading-snug font-semibold", className)}
      {...props}
    />
  );
}

function SheetDescription({ className, ...props }: React.ComponentProps<typeof SheetPrimitive.Description>) {
  return (
    <SheetPrimitive.Description
      data-slot="sheet-description"
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  );
}

export { Sheet, SheetContent, SheetDescription, SheetTitle };
