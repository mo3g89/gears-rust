import { Outlet } from 'react-router-dom';

export function SettingsLayoutPage() {
  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-xl font-semibold">Settings</h1>
        <p className="text-muted-foreground">Configure integrations and system settings</p>
      </div>
      <Outlet />
    </div>
  );
}
