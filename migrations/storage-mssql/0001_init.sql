IF OBJECT_ID(N'air_elt_cursors', N'U') IS NULL
BEGIN
    CREATE TABLE air_elt_cursors (
        flow       NVARCHAR(255) NOT NULL PRIMARY KEY,
        state      NVARCHAR(MAX) NOT NULL,
        updated_at DATETIME2 NOT NULL DEFAULT GETUTCDATE()
    );
END;
