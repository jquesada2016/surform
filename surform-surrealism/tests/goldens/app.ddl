DEFINE NAMESPACE OVERWRITE acme COMMENT 'Acme Corporation';
DEFINE CONFIG OVERWRITE DEFAULT NAMESPACE 'acme' DATABASE 'shop';
USE NS acme;
DEFINE DATABASE OVERWRITE app
    CHANGEFEED 1w INCLUDE ORIGINAL
    STRICT
    COMMENT 'Primary application database';
USE NS acme DB app;
DEFINE SEQUENCE IF NOT EXISTS invoice_number START 1000;
DEFINE PARAM OVERWRITE $default_limit VALUE 50 COMMENT 'Default page size for list endpoints';
DEFINE USER OVERWRITE deployer ON NAMESPACE
    PASSHASH '$argon2id$v=19$'
    ROLES EDITOR, VIEWER
    COMMENT 'CI deployment account';
DEFINE ANALYZER OVERWRITE `english`
    TOKENIZERS CLASS
    FILTERS SNOWBALL(ENGLISH), LOWERCASE
    COMMENT 'English text search';
DEFINE EVENT OVERWRITE user_created ON `user`
    WHEN $event = 'CREATE'
    THEN { CREATE log SET user = $after.id }
    COMMENT 'Log every user creation';
DEFINE EVENT OVERWRITE webhook_fanout ON invoice
    ASYNC RETRY 5 MAXDEPTH 8
    THEN (CREATE delivery SET invoice = $after.id),
    (UPDATE stats:webhooks SET fired = fired + 1);
DEFINE MODULE OVERWRITE mod::color FROM f"modules:/color.surli" UNSIGNED COMMENT 'Colour helpers';
DEFINE FUNCTION OVERWRITE fn::can_edit($p: record<post>) -> bool {
    RETURN $auth.id = $p.author OR $auth.role = 'admin'
}
    PERMISSIONS WHERE $auth.admin = true
    COMMENT 'Post permissions';
DEFINE CONFIG OVERWRITE API
    MIDDLEWARE api::timeout(5s), api::log()
    PERMISSIONS WHERE $auth.admin = true;
DEFINE CONFIG OVERWRITE GRAPHQL
    TABLES INCLUDE `user`, `post`
    FUNCTIONS NONE
    DEPTH 4
    COMPLEXITY 100
    INTROSPECTION NONE;
DEFINE INDEX OVERWRITE by_account_thread ON message FIELDS account, thread_id
    COMMENT 'Inbox thread lookup';
DEFINE API OVERWRITE '/admin/*rest'
    FOR ANY MIDDLEWARE api::timeout(5s) THEN { { status: 404, body: { error: 'no such route' } } }
    FOR get, delete PERMISSIONS WHERE $auth.admin = true THEN { { status: 200 } }
    COMMENT 'Admin API with fallback';
DEFINE ACCESS OVERWRITE corp_sso ON DATABASE
    TYPE JWT URL 'https://sso.acme.example/jwks'
    DURATION FOR SESSION 8h
    COMMENT 'SSO token verification';
DEFINE TABLE OVERWRITE `user` TYPE NORMAL SCHEMAFULL COMMENT 'Application users';
DEFINE FIELD OVERWRITE created_at ON `user` TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON `user` TYPE datetime;
DEFINE FIELD OVERWRITE email ON `user` TYPE string
    ASSERT string::is_email($value)
    DEFAULT 'unknown@example.com';
DEFINE FIELD OVERWRITE display_name ON `user`
    COMPUTED string::uppercase(email)
    COMMENT 'Derived display name';
