import { Loader2, Save } from 'lucide-react';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { useNotificationsForm } from './notificationsShared';

export function NotificationsEmailPage() {
  const { form, isSaving, saveEmailNotifications, setForm } = useNotificationsForm();

  return (
    <Card>
      <CardHeader>
        <CardTitle>Email Delivery</CardTitle>
        <CardDescription>
          SMTP-based notification settings shared by manual and scheduled run completion events.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="flex items-center justify-between gap-4 rounded-md border p-4">
          <div className="space-y-1">
            <Label htmlFor="notif-email-enabled" className="text-base">
              Enable Email Notifications
            </Label>
            <p className="text-sm text-muted-foreground">
              Turn on email delivery for the shared completion triggers below.
            </p>
          </div>
          <Switch
            id="notif-email-enabled"
            checked={form.email_enabled}
            onCheckedChange={(checked) =>
              setForm((current) => ({ ...current, email_enabled: checked }))
            }
          />
        </div>

        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="notif-smtp-host">SMTP Host</Label>
            <Input
              id="notif-smtp-host"
              value={form.email_smtp_host}
              onChange={(e) =>
                setForm((current) => ({ ...current, email_smtp_host: e.target.value }))
              }
              placeholder="smtp.company.com"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="notif-smtp-port">SMTP Port</Label>
            <Input
              id="notif-smtp-port"
              type="number"
              min={1}
              value={form.email_smtp_port}
              onChange={(e) =>
                setForm((current) => ({
                  ...current,
                  email_smtp_port: Number(e.target.value) || 25,
                }))
              }
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="notif-email-from">From</Label>
            <Input
              id="notif-email-from"
              value={form.email_from}
              onChange={(e) =>
                setForm((current) => ({ ...current, email_from: e.target.value }))
              }
              placeholder="vhp-tests@company.com"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="notif-email-recipients">Recipients</Label>
            <Input
              id="notif-email-recipients"
              value={form.email_recipients}
              onChange={(e) =>
                setForm((current) => ({ ...current, email_recipients: e.target.value }))
              }
              placeholder="qa@company.com,team@company.com"
            />
            <p className="text-xs text-muted-foreground">Use commas to separate multiple recipients.</p>
          </div>
        </div>

        <div className="flex justify-end">
          <Button onClick={saveEmailNotifications} disabled={isSaving}>
            {isSaving ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : (
              <Save className="mr-2 h-4 w-4" />
            )}
            Save Email Notifications
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
