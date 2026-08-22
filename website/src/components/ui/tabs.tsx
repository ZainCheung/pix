import {
  createContext,
  useContext,
  useId,
  useState,
  type ButtonHTMLAttributes,
  type HTMLAttributes,
} from 'react'

import { cn } from '#/lib/utils'

type TabsContextValue = {
  id: string
  value: string
  setValue: (value: string) => void
}

const TabsContext = createContext<TabsContextValue | null>(null)

function useTabsContext() {
  const context = useContext(TabsContext)
  if (!context) throw new Error('Tabs components must be used inside Tabs')
  return context
}

type TabsProps = HTMLAttributes<HTMLDivElement> & {
  defaultValue?: string
  onValueChange?: (value: string) => void
  variant?: 'default' | 'line'
  value?: string
}

export function Tabs({
  className,
  defaultValue = '',
  onValueChange,
  variant = 'default',
  value: controlledValue,
  ...props
}: TabsProps) {
  const [uncontrolledValue, setUncontrolledValue] = useState(defaultValue)
  const value = controlledValue ?? uncontrolledValue
  const id = useId()

  function setValue(nextValue: string) {
    if (controlledValue === undefined) setUncontrolledValue(nextValue)
    onValueChange?.(nextValue)
  }

  return (
    <TabsContext.Provider value={{ id, value, setValue }}>
      <div className={cn('tabs', className)} data-variant={variant} {...props} />
    </TabsContext.Provider>
  )
}

export function TabsList({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn('tabs-list', className)} role="tablist" {...props} />
}

type TabsTriggerProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  value: string
}

export function TabsTrigger({ className, value, onClick, onKeyDown, ...props }: TabsTriggerProps) {
  const context = useTabsContext()
  const active = context.value === value
  const tabId = `${context.id}-tab-${value.replace(/[^a-zA-Z0-9_-]/g, '-')}`
  const panelId = `${context.id}-panel-${value.replace(/[^a-zA-Z0-9_-]/g, '-')}`

  return (
    <button
      {...props}
      className={cn('tabs-trigger', className)}
      type="button"
      role="tab"
      id={tabId}
      aria-controls={panelId}
      aria-selected={active}
      data-state={active ? 'active' : 'inactive'}
      data-tab-value={value}
      tabIndex={active ? 0 : -1}
      onClick={(event) => {
        onClick?.(event)
        if (!event.defaultPrevented && !props.disabled) context.setValue(value)
      }}
      onKeyDown={(event) => {
        onKeyDown?.(event)
        if (event.defaultPrevented || !['ArrowRight', 'ArrowLeft', 'Home', 'End'].includes(event.key)) return

        const tabs = Array.from(
          event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]:not(:disabled)') ?? [],
        )
        const currentIndex = tabs.indexOf(event.currentTarget)
        if (currentIndex < 0 || tabs.length === 0) return

        const nextIndex = event.key === 'Home'
          ? 0
          : event.key === 'End'
            ? tabs.length - 1
            : (currentIndex + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length
        const nextTab = tabs[nextIndex]
        nextTab.focus()
        context.setValue(nextTab.dataset.tabValue ?? value)
        event.preventDefault()
      }}
    />
  )
}

type TabsContentProps = HTMLAttributes<HTMLDivElement> & {
  value: string
}

export function TabsContent({ className, value, ...props }: TabsContentProps) {
  const context = useTabsContext()
  const key = value.replace(/[^a-zA-Z0-9_-]/g, '-')
  const active = context.value === value

  return (
    <div
      {...props}
      className={cn('tabs-content', className)}
      role="tabpanel"
      id={`${context.id}-panel-${key}`}
      aria-labelledby={`${context.id}-tab-${key}`}
      hidden={!active}
      tabIndex={0}
    />
  )
}
