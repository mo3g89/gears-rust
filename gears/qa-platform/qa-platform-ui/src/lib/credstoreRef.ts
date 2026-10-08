/** Credstore's `SecretRef` syntax, which every gear applies to a credential reference:
 *  letters, digits, `_` and `-`, 1 to 255 characters, no scheme prefix. */
export const CREDSTORE_REF_PATTERN = /^[A-Za-z0-9_-]{1,255}$/;

/** Example names shown as input placeholders on the settings pages. */
export const CREDSTORE_REF_PLACEHOLDERS = {
  slackWebhook: 'qa-slack-webhook',
  smtpPassword: 'qa-smtp-password',
  jiraApiToken: 'qa-jira-api-token',
} as const;
