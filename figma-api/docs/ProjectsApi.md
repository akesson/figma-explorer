# \ProjectsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_project_files**](ProjectsApi.md#get_project_files) | **GET** /v1/projects/{project_id}/files | Get files in a project
[**get_team_projects**](ProjectsApi.md#get_team_projects) | **GET** /v1/teams/{team_id}/projects | Get projects in a team



## get_project_files

> models::InlineObject6 get_project_files(project_id, branch_data)
Get files in a project

Get a list of all the Files within the specified project.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**project_id** | **String** | ID of the project to list files from | [required] |
**branch_data** | Option<**bool**> | Returns branch metadata in the response for each main file with a branch inside the project. |  |[default to false]

### Return type

[**models::InlineObject6**](inline_object_6.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_team_projects

> models::InlineObject5 get_team_projects(team_id)
Get projects in a team

You can use this endpoint to get a list of all the Projects within the specified team. This will only return projects visible to the authenticated user or owner of the developer token. Note: it is not currently possible to programmatically obtain the team id of a user just from a token. To obtain a team id, navigate to a team page of a team you are a part of. The team id will be present in the URL after the word team and before your team name.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**team_id** | **String** | ID of the team to list projects from | [required] |

### Return type

[**models::InlineObject5**](inline_object_5.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

