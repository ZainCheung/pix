import type { AnchorHTMLAttributes, ButtonHTMLAttributes } from 'react'

import { cn } from '#/lib/utils'

type Variant = 'primary' | 'secondary' | 'quiet'

const variantClass: Record<Variant, string> = {
  primary: 'button button-primary',
  secondary: 'button button-secondary',
  quiet: 'button button-quiet',
}

export function buttonClass(variant: Variant = 'secondary') {
  return variantClass[variant]
}

export function ButtonLink({
  className,
  variant = 'secondary',
  ...props
}: AnchorHTMLAttributes<HTMLAnchorElement> & { variant?: Variant }) {
  return <a className={cn(buttonClass(variant), className)} {...props} />
}

export function Button({
  className,
  variant = 'secondary',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return <button className={cn(buttonClass(variant), className)} {...props} />
}
