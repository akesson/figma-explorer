# GetDeveloperLogsRequest

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**token_type** | Option<**String**> | Filter by the type of token used for authentication. | [optional]
**token** | Option<**String**> | Filter by token value(s). Multiple values can be separated by commas. | [optional]
**token_name** | Option<**String**> | Filter by token name prefix(es). Multiple values can be separated by commas. | [optional]
**user_email** | Option<**String**> | Filter by user email prefix(es). Multiple values can be separated by commas. | [optional]
**ip_address** | Option<**String**> | Filter by IP address prefix(es). Multiple values can be separated by commas. | [optional]
**event_source** | Option<**String**> | Filter by event source. | [optional]
**date_range** | Option<**String**> | Filter by date range. | [optional][default to Last30d]
**limit** | Option<**u8**> | Maximum number of entries to return. | [optional][default to 25]
**cursor** | Option<**String**> | A cursor returned from a previous request, used for pagination. | [optional]

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


