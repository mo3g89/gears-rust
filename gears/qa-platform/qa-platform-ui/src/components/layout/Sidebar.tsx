import { Link, useLocation } from 'react-router-dom';
import {
  LayoutDashboard,
  FileText,
  PlayCircle,
  Calendar,
  Menu,
  Server,
  Package,
  Settings,
  BarChart3,
  Bell,
  LogOut,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Separator } from '@/components/ui/separator';
import { ProductSwitcher } from '@/components/filters/ProductSwitcher';
import { useAuth } from '@/auth';

interface NavItem {
  name: string;
  href: string;
  icon: React.ComponentType<{ className?: string }>;
  children?: Array<{ name: string; href: string }>;
}

const navItems: NavItem[] = [
  { name: 'Dashboard', href: '/', icon: LayoutDashboard },
  { name: 'Test Plans', href: '/plans', icon: FileText },
  { name: 'Test Runs', href: '/runs', icon: PlayCircle },
  { name: 'Schedules', href: '/schedules', icon: Calendar },
  { name: 'Analytics', href: '/analytics', icon: BarChart3 },
  { name: 'Environments', href: '/environments', icon: Server },
  { name: 'Product Catalog', href: '/products', icon: Package },
  {
    name: 'Notifications',
    href: '/notifications',
    icon: Bell,
    children: [
      { name: 'Email', href: '/notifications/email' },
      { name: 'Slack', href: '/notifications/slack' },
    ],
  },
  {
    name: 'Settings',
    href: '/settings',
    icon: Settings,
    children: [
      { name: 'Variables', href: '/settings/variables' },
      { name: 'SSH Keys', href: '/settings/ssh-keys' },
      { name: 'JIRA', href: '/settings/jira' },
    ],
  },
];

interface SidebarProps {
  isOpen?: boolean;
  onClose?: () => void;
}

export function Sidebar({ isOpen = true, onClose }: SidebarProps) {
  const location = useLocation();
  const { user, logout } = useAuth();

  return (
    <aside
      className={cn(
        'fixed inset-y-0 left-0 z-50 w-64 bg-card border-r transition-transform duration-300 lg:translate-x-0',
        isOpen ? 'translate-x-0' : '-translate-x-full'
      )}
    >
      <div className="flex flex-col h-full">
        {/* Header: logo + global product switcher */}
        <div className="flex items-center gap-2 px-4 py-4">
          <img
            src="/virtuozzo-logo.png"
            alt="Virtuozzo Logo"
            className="h-8 w-8 shrink-0 object-contain"
          />
          <ProductSwitcher className="flex-1 min-w-0" />
          {onClose && (
            <Button variant="ghost" size="icon" onClick={onClose} className="lg:hidden shrink-0">
              <Menu className="h-5 w-5" />
            </Button>
          )}
        </div>

        <Separator />

        {/* Navigation */}
        <nav className="flex-1 px-4 py-4 space-y-1">
          {navItems.map((item) => {
            const Icon = item.icon;
            const isActive = location.pathname === item.href ||
              (item.href !== '/' && location.pathname.startsWith(item.href));
            const showChildren = Boolean(item.children?.length) && isActive;

            return (
              <div key={item.href} className="space-y-1">
                <Link
                  to={item.href}
                  className={cn(
                    'flex items-center gap-3 px-3 py-2 rounded-md text-sm font-medium transition-colors',
                    isActive
                      ? 'bg-primary text-primary-foreground'
                      : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                  )}
                >
                  <Icon className="h-5 w-5" />
                  {item.name}
                </Link>

                {showChildren && item.children?.length ? (
                  <div className="ml-6 border-l pl-2 space-y-1">
                    {item.children.map((child) => {
                      const childActive = location.pathname === child.href ||
                        location.pathname.startsWith(`${child.href}/`);
                      return (
                        <Link
                          key={child.href}
                          to={child.href}
                          className={cn(
                            'block px-2 py-1 rounded text-xs transition-colors',
                            childActive
                              ? 'bg-primary/15 text-primary font-medium'
                              : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                          )}
                        >
                          {child.name}
                        </Link>
                      );
                    })}
                  </div>
                ) : null}
              </div>
            );
          })}
        </nav>

        <Separator />

        {/* Footer.

            The signed-in identity and the logout control are the ONLY addition
            this file received in Task 15; everything above is the copied-verbatim
            component. `preferred_username` is the claim Keycloak puts the login
            name in (`admin`/`viewer` on the qa-platform realm); `sub` is the
            fallback because a realm is free not to map it. */}
        <div className="px-6 py-4 space-y-2 text-xs text-muted-foreground">
          {user ? (
            <div className="flex items-center justify-between gap-2">
              <span className="truncate" title={String(user.profile.preferred_username ?? user.profile.sub)}>
                {String(user.profile.preferred_username ?? user.profile.sub)}
              </span>
              <Button
                variant="ghost"
                size="sm"
                onClick={logout}
                title="Sign out"
                className="shrink-0 h-7 px-2"
              >
                <LogOut className="h-4 w-4" />
                Sign out
              </Button>
            </div>
          ) : null}
          <p>QA Platform v1.0.0</p>
        </div>
      </div>
    </aside>
  );
}
