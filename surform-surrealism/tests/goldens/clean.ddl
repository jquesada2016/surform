DEFINE NAMESPACE OVERWRITE acme COMMENT 'Acme Corporation';
USE NS acme;
DEFINE NAMESPACE OVERWRITE staging;
USE NS staging;
DEFINE SEQUENCE OVERWRITE invoice_number START 1000;
DEFINE PARAM OVERWRITE $default_limit VALUE 50 COMMENT 'Default page size for list endpoints';
