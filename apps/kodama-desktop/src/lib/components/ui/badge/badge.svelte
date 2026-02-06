<script lang="ts" module>
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { tv, type VariantProps } from "tailwind-variants";

  export const badgeVariants = tv({
    base: "inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-semibold transition-colors focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2",
    variants: {
      variant: {
        default: "border-transparent bg-primary text-primary-foreground",
        secondary: "border-transparent bg-secondary text-secondary-foreground",
        destructive: "border-transparent bg-destructive text-destructive-foreground",
        outline: "text-foreground",
        success: "border-transparent bg-green-900/50 text-green-400",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  });

  export type BadgeVariant = VariantProps<typeof badgeVariants>["variant"];
</script>

<script lang="ts">
  import { cn } from "$lib/utils.js";

  let {
    class: className,
    variant = "default",
    children,
    ...restProps
  }: HTMLAttributes<HTMLDivElement> & {
    variant?: BadgeVariant;
    children?: Snippet;
  } = $props();
</script>

<div class={cn(badgeVariants({ variant }), className)} {...restProps}>
  {#if children}
    {@render children()}
  {/if}
</div>
