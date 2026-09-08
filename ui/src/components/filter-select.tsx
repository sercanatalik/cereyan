import { Check, ChevronDown } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

export interface FilterOption {
  value: string;
  label: string;
}

/**
 * A filter as a labelled popover button: "State · Any" opens a searchable list.
 * An empty value means "no filter" and shows `anyLabel`.
 */
export function FilterSelect({
  label,
  value,
  options,
  onChange,
  anyLabel = "Any",
  allowAny = true,
  searchable,
  className,
  "aria-label": ariaLabel,
}: {
  label: string;
  value: string;
  options: FilterOption[];
  onChange: (value: string) => void;
  anyLabel?: string;
  /** Offer an "any" row that clears the filter. Off for sorts and ranges. */
  allowAny?: boolean;
  searchable?: boolean;
  className?: string;
  "aria-label"?: string;
}) {
  const [open, setOpen] = useState(false);
  const current = options.find((o) => o.value === value);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          className={cn("gap-2 font-medium", value && "border-foreground/30", className)}
          aria-label={ariaLabel ?? label}
          data-testid={`filter-${label.toLowerCase().replace(/\s+/g, "-")}`}
          data-value={value}
        >
          <span className="font-normal text-muted-foreground">{label}</span>
          {current?.label ?? (value || anyLabel)}
          <ChevronDown className="size-3.5 text-muted-foreground" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-56 p-0" align="start">
        <Command>
          {(searchable ?? options.length > 8) ? (
            <CommandInput placeholder={`Filter ${label.toLowerCase()}`} />
          ) : null}
          <CommandList>
            <CommandEmpty>No match.</CommandEmpty>
            <CommandGroup>
              {allowAny ? (
                <CommandItem
                  value="__any"
                  onSelect={() => {
                    onChange("");
                    setOpen(false);
                  }}
                >
                  <Check className={cn("size-4", value ? "opacity-0" : "opacity-100")} />
                  {anyLabel}
                </CommandItem>
              ) : null}
              {options.map((o) => (
                <CommandItem
                  key={o.value}
                  value={o.label}
                  onSelect={() => {
                    onChange(o.value);
                    setOpen(false);
                  }}
                >
                  <Check className={cn("size-4", o.value === value ? "opacity-100" : "opacity-0")} />
                  {o.label}
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
