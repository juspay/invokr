import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docsSidebar: [
    {
      type: 'category',
      label: 'Getting Started',
      collapsible: false,
      items: [
        'intro',
        'quickstart',
      ],
    },
    {
      type: 'category',
      label: 'Core Concepts',
      collapsible: true,
      collapsed: false,
      items: [
        'core-concepts/overview',
        'core-concepts/jobs',
        'core-concepts/executions',
        'core-concepts/endpoints',
        'core-concepts/payload-specs',
        'core-concepts/configs',
        'core-concepts/secrets',
        'core-concepts/templates',
        'core-concepts/retry-policy',
        'core-concepts/idempotency',
        'core-concepts/multi-tenancy',
      ],
    },
    {
      type: 'category',
      label: 'Guides',
      collapsible: true,
      collapsed: true,
      items: [
        'guides/http-endpoints',
        'guides/kafka-endpoints',
        'guides/redis-stream-endpoints',
        'guides/cron-jobs',
        'guides/delayed-jobs',
        'guides/versioning',
        'guides/pagination',
        'guides/monitoring',
      ],
    },
    {
      type: 'category',
      label: 'API Reference',
      collapsible: true,
      collapsed: true,
      items: [
        'api/invokr/invokr-api',
        {
          type: 'category',
          label: 'Management API',
          items: [
            'api/management/organizations',
            'api/management/workspaces',
            'api/management/configs',
            'api/management/secrets',
          ],
        },
        {
          type: 'category',
          label: 'Payload Specs',
          items: [
            'api/invokr/create-payload-spec',
            'api/invokr/list-payload-specs',
            'api/invokr/get-payload-spec',
            'api/invokr/update-payload-spec',
            'api/invokr/delete-payload-spec',
          ],
        },
        {
          type: 'category',
          label: 'Endpoints',
          items: [
            'api/invokr/create-endpoint',
            'api/invokr/list-endpoints',
            'api/invokr/get-endpoint',
            'api/invokr/update-endpoint',
            'api/invokr/delete-endpoint',
          ],
        },
        {
          type: 'category',
          label: 'Jobs',
          items: [
            'api/invokr/create-job',
            'api/invokr/list-jobs',
            'api/invokr/get-job',
            'api/invokr/update-job',
            'api/invokr/cancel-job',
            'api/invokr/get-job-status',
            'api/invokr/get-job-versions',
            'api/invokr/list-job-executions',
          ],
        },
        {
          type: 'category',
          label: 'Executions',
          items: [
            'api/invokr/get-execution',
            'api/invokr/cancel-execution',
            'api/invokr/list-execution-attempts',
            'api/invokr/list-execution-logs',
          ],
        },
      ],
    },
    {
      type: 'category',
      label: 'Architecture',
      collapsible: true,
      collapsed: true,
      items: [
        'architecture/overview',
        'architecture/worker-pipeline',
        'architecture/db-driven-scheduling',
        'architecture/exactly-once',
        'architecture/reaper',
        'architecture/dual-deployment',
        'architecture/database-schema',
      ],
    },
    {
      type: 'category',
      label: 'SDKs',
      collapsible: true,
      collapsed: true,
      items: [
        'sdks/overview',
        'sdks/typescript',
        'sdks/rust',
        'sdks/haskell',
      ],
    },
    {
      type: 'category',
      label: 'Deployment',
      collapsible: true,
      collapsed: true,
      items: [
        'deployment/docker',
        'deployment/library-mode',
        'deployment/production',
        'deployment/secrets-management',
        'deployment/dashboard',
      ],
    },
    {
      type: 'category',
      label: 'Configuration',
      collapsible: true,
      collapsed: true,
      items: [
        'configuration/environment-variables',
      ],
    },
    {
      type: 'category',
      label: 'Development',
      collapsible: true,
      collapsed: true,
      items: [
        'development/setup',
        'development/building',
        'development/testing',
        'development/sdk-codegen',
      ],
    },
  ],

  apiSidebar: [
    'api/invokr/invokr-api',
    {
      type: 'category',
      label: 'Management API',
      items: [
        'api/management/organizations',
        'api/management/workspaces',
        'api/management/configs',
        'api/management/secrets',
      ],
    },
    {
      type: 'category',
      label: 'Payload Specs',
      items: [
        'api/invokr/create-payload-spec',
        'api/invokr/list-payload-specs',
        'api/invokr/get-payload-spec',
        'api/invokr/update-payload-spec',
        'api/invokr/delete-payload-spec',
      ],
    },
    {
      type: 'category',
      label: 'Endpoints',
      items: [
        'api/invokr/create-endpoint',
        'api/invokr/list-endpoints',
        'api/invokr/get-endpoint',
        'api/invokr/update-endpoint',
        'api/invokr/delete-endpoint',
      ],
    },
    {
      type: 'category',
      label: 'Jobs',
      items: [
        'api/invokr/create-job',
        'api/invokr/list-jobs',
        'api/invokr/get-job',
        'api/invokr/update-job',
        'api/invokr/cancel-job',
        'api/invokr/get-job-status',
        'api/invokr/get-job-versions',
        'api/invokr/list-job-executions',
      ],
    },
    {
      type: 'category',
      label: 'Executions',
      items: [
        'api/invokr/get-execution',
        'api/invokr/cancel-execution',
        'api/invokr/list-execution-attempts',
        'api/invokr/list-execution-logs',
      ],
    },
  ],
};

export default sidebars;
