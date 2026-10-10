import type { ButtonHTMLAttributes, ReactNode } from "react";

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement>;

function Button({
  className = "",
  type = "button",
  ...props
}: ButtonProps & { className?: string }) {
  return (
    <button
      {...props}
      className={`button ${className}`.trim()}
      type={type}
    />
  );
}

export function PrimaryButton(props: ButtonProps) {
  return <Button {...props} className="button--primary" />;
}

export function SecondaryButton(props: ButtonProps) {
  return <Button {...props} className="button--secondary" />;
}

export function DestructiveButton(props: ButtonProps) {
  return <Button {...props} className="button--destructive" />;
}

export function TertiaryButton(props: ButtonProps) {
  return <Button {...props} className="button--tertiary" />;
}

export function IconButton({
  children,
  ...props
}: Omit<ButtonProps, "aria-label"> & { "aria-label": string; children: ReactNode }) {
  return (
    <Button {...props} className="button--icon">
      {children}
    </Button>
  );
}
