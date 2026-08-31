import { useState } from 'react';
import { toast } from 'sonner';
import { Loader2, Save, Trash2 } from 'lucide-react';
import { useSshKeys, useCreateSshKey, useDeleteSshKey } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Label } from '@/components/ui/label';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { Button } from '@/components/ui/button';
import { useConfirm } from '@/components/ui/confirm-dialog';

export function SettingsSshKeysPage() {
  const { data: sshKeys, isLoading } = useSshKeys();
  const createSshKey = useCreateSshKey();
  const deleteSshKey = useDeleteSshKey();
  const confirm = useConfirm();
  const [form, setForm] = useState({
    name: '',
    private_key: '',
  });

  const save = () => {
    if (!form.name.trim() || !form.private_key.trim()) {
      toast.error('SSH key name and private key are required');
      return;
    }

    createSshKey.mutate(
      {
        name: form.name.trim(),
        private_key: form.private_key,
      },
      {
        onSuccess: () => {
          toast.success('SSH key added');
          setForm({ name: '', private_key: '' });
        },
        onError: (err) => toast.error('Failed to add SSH key', { description: String(err) }),
      }
    );
  };

  const remove = async (id: string, name: string) => {
    const ok = await confirm({
      title: `Delete SSH key "${name}"?`,
      description: 'This SSH key will be removed permanently.',
      confirmText: 'Delete',
      variant: 'destructive',
    });
    if (!ok) return;
    deleteSshKey.mutate(id, {
      onSuccess: () => toast.success('SSH key deleted'),
      onError: (err) => toast.error('Failed to delete SSH key', { description: String(err) }),
    });
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>SSH Keys</CardTitle>
        <CardDescription>Manage private keys used for cloning SSH test repositories.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="space-y-4 rounded-md border p-4">
          <div className="space-y-2">
            <Label htmlFor="ssh-key-name">Key Name</Label>
            <Input
              id="ssh-key-name"
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="Bitbucket Deploy Key"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="ssh-private-key">Private Key</Label>
            <Textarea
              id="ssh-private-key"
              value={form.private_key}
              onChange={(e) => setForm({ ...form, private_key: e.target.value })}
              placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
              rows={8}
            />
          </div>
          <Button onClick={save} disabled={createSshKey.isPending}>
            {createSshKey.isPending ? <Loader2 className="h-4 w-4 animate-spin mr-2" /> : <Save className="h-4 w-4 mr-2" />}
            Add SSH Key
          </Button>
        </div>

        <div className="space-y-2">
          <Label>Configured Keys</Label>
          {!sshKeys?.length ? (
            <p className="text-sm text-muted-foreground">No SSH keys configured</p>
          ) : (
            <div className="space-y-2">
              {sshKeys.map((key) => (
                <div key={key.id} className="flex items-center justify-between rounded-md border px-3 py-2">
                  <div>
                    <p className="text-sm font-medium">{key.name}</p>
                    <p className="text-xs text-muted-foreground">{new Date(key.updated_at).toLocaleString()}</p>
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => remove(key.id, key.name)}
                    disabled={deleteSshKey.isPending}
                  >
                    <Trash2 className="h-4 w-4 text-destructive" />
                  </Button>
                </div>
              ))}
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
